//! `devctx models` — see what can be embedded with, and fetch what needs fetching.
//!
//! The choice of embedding model is the one decision that cannot be revisited
//! cheaply: it fixes the width of every vector in the store, so changing it
//! means re-indexing every repository and re-embedding every memory. It
//! deserves to be visible *before* anything is indexed, which is what listing
//! them here is for.
//!
//! Two kinds of model exist, and the difference is invisible until it bites.
//! Most are built into fastembed and download themselves on first use. Granite
//! is not: it is loaded as a user-defined ONNX model from a directory the user
//! provides, and without that directory it fails to load — clearly, but only at
//! the moment someone tries to index. `models download` fetches it so that
//! moment never arrives.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use devctx_embed::registry::{find_local, LOCAL_MODELS};

/// What a model is good for, in the terms the choice is actually made in.
/// Keyed by registry key so it stays beside the registry without living in it —
/// `devctx-embed` is deliberately free of prose.
fn notes(key: &str) -> (&'static str, &'static str) {
    match key {
        "minilm-l6" => ("English", "smallest and fastest; the built-in default"),
        "minilm-l12" => ("English", "slightly better than L6, still light"),
        "bge-small" => ("English", "better English retrieval than MiniLM"),
        "bge-base" => (
            "English",
            "best English precision; 768-wide, so a larger store",
        ),
        "ml-minilm" => ("50+ languages", "fast multilingual, no files to fetch"),
        "ml-mpnet" => ("50+ languages", "768-wide; 128-token input cap"),
        "ml-granite" => (
            "multilingual",
            "best multilingual on CPU: top recall, fastest indexing",
        ),
        "ml-granite-lg" => (
            "multilingual",
            "768-wide sibling; ml-granite matches it on CPU",
        ),
        _ => ("—", ""),
    }
}

/// The files a user-defined ONNX model needs on disk, in the layout the loader
/// expects. The ONNX itself is tried in order: the quantized build first,
/// because it is the one worth having and a fifth of the size.
const ONNX_CANDIDATES: &[&str] = &["onnx/model_quint8_avx2.onnx", "onnx/model.onnx"];
const TOKENIZER_FILES: &[(&str, bool)] = &[
    ("tokenizer.json", true),
    ("config.json", true),
    ("special_tokens_map.json", false),
    ("tokenizer_config.json", false),
];

/// `devctx models` — list what can be configured, and what each one costs.
pub fn list(configured: Option<&str>) -> Result<()> {
    println!(
        "{:<15} {:>5}  {:<14} {:<10} NOTES",
        "MODEL", "DIMS", "LANGUAGES", "FILES"
    );
    for spec in LOCAL_MODELS {
        let (langs, note) = notes(spec.key);
        let files = if spec.builtin.is_some() {
            "automatic"
        } else if local_dir(spec.key).is_some() {
            "ready"
        } else {
            "download"
        };
        let mark = if configured == Some(spec.key) {
            "*"
        } else {
            " "
        };
        println!(
            "{mark}{:<14} {:>5}  {:<14} {:<10} {}",
            spec.key, spec.dimension, langs, files, note
        );
    }
    println!();
    if let Some(c) = configured {
        println!("* currently configured for new projects (`{c}`).");
    }
    println!(
        "FILES: `automatic` downloads itself on first use; `download` needs\n\
         `devctx models download <model>` once; `ready` is already on this machine.\n\
         \n\
         Changing the model after indexing means re-indexing everything and\n\
         re-embedding every memory, so choose before the first `devctx index`.\n\
         Non-English code or comments? Pick a multilingual one: the English\n\
         models embed Spanish perfectly happily, just badly."
    );
    Ok(())
}

/// `devctx models download <key>` — fetch a user-defined ONNX model.
pub fn download(key: &str) -> Result<PathBuf> {
    let spec = find_local(key).ok_or_else(|| {
        anyhow!("unknown model `{key}`; run `devctx models` to see what there is")
    })?;
    if spec.builtin.is_some() {
        bail!(
            "`{key}` needs no download: it is built in and fetched on first use. \
             Only Granite-style models are downloaded here."
        );
    }
    let dir = target_dir(key)?;
    std::fs::create_dir_all(dir.join("onnx"))
        .with_context(|| format!("creating {}", dir.display()))?;

    // stderr: progress a caller pipes away should still reach a person.
    eprintln!("Fetching {} into {}", spec.hf_repo, dir.display());

    let mut got_onnx = false;
    for candidate in ONNX_CANDIDATES {
        let url = hf_url(spec.hf_repo, candidate);
        eprintln!("  {candidate} …");
        match fetch(&url, &dir.join(candidate)) {
            Ok(bytes) => {
                eprintln!("  {candidate} ({} MB)", bytes / 1_048_576);
                got_onnx = true;
                break;
            }
            // The quantized build does not exist for every repo; that is not an
            // error until none of them do.
            Err(_) => continue,
        }
    }
    if !got_onnx {
        bail!(
            "no ONNX file found in {} (tried {ONNX_CANDIDATES:?}). The repository \
             may not publish an ONNX export; fetch it by hand and point \
             `embeddings.model_dir` at the directory.",
            spec.hf_repo
        );
    }

    for (file, required) in TOKENIZER_FILES {
        match fetch(&hf_url(spec.hf_repo, file), &dir.join(file)) {
            Ok(_) => eprintln!("  {file}"),
            Err(e) if *required => {
                return Err(e).with_context(|| format!("{file} is required by the loader"))
            }
            Err(_) => {}
        }
    }

    println!(
        "\nReady. Point a project at it with:\n    \
         embeddings:\n      model: {key}\n      model_dir: {}",
        dir.display()
    );
    Ok(dir)
}

/// Ask which model to use, offering the registry and fetching what is chosen.
///
/// Returns `None` when there is nobody to ask — no terminal, which is the case
/// for a script or an agent — so the caller falls back to the machine default
/// rather than blocking forever on a prompt no one will answer.
///
/// Asking at all is the point: this decision is made once per machine, cannot be
/// revisited without re-indexing, and its wrong answers are silent. Someone
/// running `devctx init` in a Spanish codebase should be shown that the default
/// is English-only *before* the first index, not discover it in poor results
/// months later.
pub fn prompt(default_key: &str) -> Result<Option<String>> {
    use std::io::{IsTerminal as _, Write as _};
    if !std::io::stdin().is_terminal() {
        return Ok(None);
    }
    list(Some(default_key))?;
    print!("\nModel to use [{default_key}]: ");
    std::io::stdout().flush().ok();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Ok(None);
    }
    let key = match line.trim() {
        "" => default_key.to_string(),
        chosen => chosen.to_string(),
    };
    let spec = find_local(&key).ok_or_else(|| {
        anyhow!("unknown model `{key}`; run `devctx models` to see what there is")
    })?;

    // Fetch it now rather than letting the first index fail on a missing
    // directory: the answer was given here, so the consequence belongs here.
    if spec.builtin.is_none() && local_dir(&key).is_none() {
        eprintln!("`{key}` needs its files; fetching them now.");
        download(&key)?;
    }
    Ok(Some(key))
}

/// Where a downloaded model lives: the shared model cache, one directory per
/// key. Shared on purpose — the files are identical whoever asks, and run to
/// hundreds of megabytes.
pub fn target_dir(key: &str) -> Result<PathBuf> {
    let base = devctx_core::dirs::model_cache_dir()
        .ok_or_else(|| anyhow!("no home directory to cache models in"))?;
    Ok(base.join(key))
}

/// The directory of an already-downloaded model, if it is usable.
pub fn local_dir(key: &str) -> Option<PathBuf> {
    let dir = target_dir(key).ok()?;
    let has_onnx = ONNX_CANDIDATES.iter().any(|c| dir.join(c).is_file());
    (has_onnx && dir.join("tokenizer.json").is_file()).then_some(dir)
}

fn hf_url(repo: &str, file: &str) -> String {
    format!("https://huggingface.co/{repo}/resolve/main/{file}")
}

/// Download one file, returning its size.
///
/// Streamed to disk rather than read into memory: these run to hundreds of
/// megabytes, and buffering one whole meant nothing appeared on disk until the
/// transfer finished — a download that looked, for minutes, exactly like a
/// hang. Written to a temporary name and renamed on success, so an interrupted
/// one never leaves a half file that the loader would take for a model.
fn fetch(url: &str, dest: &Path) -> Result<u64> {
    let resp = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(30))
        .build()
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let tmp = dest.with_extension("partial");
    let mut out =
        std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let n = std::io::copy(&mut resp.into_reader(), &mut out)
        .with_context(|| format!("downloading {url}"))?;
    std::fs::rename(&tmp, dest).with_context(|| format!("renaming into {}", dest.display()))?;
    Ok(n)
}

/// The release asset name for the platform this binary was built for.
///
/// Derived from compile-time target facts rather than `uname`: a binary knows
/// what it is, and asking the operating system invites installing an x86 build
/// on an ARM machine because both answer "Linux".
pub fn target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

/// `devctx update` — replace this binary with the latest published release.
///
/// Downloads beside the running executable and renames over it, which is atomic
/// on the same filesystem and safe while the old one runs: the kernel keeps the
/// open image alive, so a server mid-index is not killed by an upgrade — it
/// simply keeps running the code it started with until restarted.
pub fn self_update(repo: &str, current: &str) -> Result<()> {
    let target = target_triple().ok_or_else(|| {
        anyhow!(
            "no published build for {}-{}; update by rebuilding from source",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let latest: String = ureq::get(&format!(
        "https://api.github.com/repos/{repo}/releases/latest"
    ))
    .set("User-Agent", "devctx")
    .call()
    .context("asking GitHub for the latest release")?
    .into_json::<serde_json::Value>()
    .context("reading the release")?
    .get("tag_name")
    .and_then(|t| t.as_str())
    .map(str::to_string)
    .ok_or_else(|| anyhow!("the latest release has no tag"))?;

    if latest.trim_start_matches('v') == current {
        println!("Already on {current} (latest is {latest}).");
        return Ok(());
    }
    println!("Updating {current} → {latest}");

    let exe = std::env::current_exe().context("locating this binary")?;
    let dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    let tmp = dir.join(".devctx-update");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).with_context(|| format!("creating {}", tmp.display()))?;

    let name = format!("devctx-{target}");
    let url = format!("https://github.com/{repo}/releases/download/{latest}/{name}.tar.gz");
    let archive = tmp.join("devctx.tar.gz");
    fetch(&url, &archive).with_context(|| format!("downloading {url}"))?;

    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&tmp)
        .status()
        .context("running tar")?;
    if !status.success() {
        bail!("could not unpack {}", archive.display());
    }
    let fresh = tmp.join(&name).join("devctx");
    if !fresh.is_file() {
        bail!("the archive did not contain a devctx binary");
    }
    // Same directory, so this is a rename rather than a copy across devices.
    std::fs::rename(&fresh, &exe).with_context(|| format!("replacing {}", exe.display()))?;
    let _ = std::fs::remove_dir_all(&tmp);

    println!("Updated {}.", exe.display());
    println!(
        "Running servers keep the old code until restarted: `devctx serve --stop` \
         in each project, and reconnect any MCP client."
    );
    Ok(())
}
