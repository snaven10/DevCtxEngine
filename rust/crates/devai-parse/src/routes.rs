//! Framework-aware HTTP route extraction (regex-based).
//!
//! Best-effort extractors for FastAPI, Flask, Express, NestJS, Spring, Quarkus
//! (JAX-RS) and Angular router configs. Mirrors the legacy `*_routes.py`
//! (rewrite plan §4). The framework is auto-detected from extension + content;
//! class-level prefixes are applied where a single one is evident.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

/// An extracted route (a handler mapped to an HTTP method + path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Framework the route was found in.
    pub framework: String,
    /// HTTP method (uppercased), or empty (e.g. Angular client routes).
    pub http_method: String,
    /// Route path (with class-level prefix applied when known).
    pub path: String,
    /// Handler class, if known.
    pub handler_class: String,
    /// Handler method/function, if known.
    pub handler_method: String,
    /// `Class.method` or `method`, for reverse lookup.
    pub handler_symbol: String,
    /// Source file.
    pub file: String,
    /// 1-based line.
    pub line: u32,
}

/// Extract routes from `source`, auto-detecting the framework by `path` + content.
pub fn extract_routes(source: &str, path: &Path) -> Vec<Route> {
    let file = path.to_string_lossy().to_string();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "py" => {
            if source.contains("APIRouter")
                || source.contains("FastAPI")
                || RE_FASTAPI.is_match(source)
            {
                fastapi(source, &file)
            } else if source.contains(".route(") || source.contains("Flask") {
                flask(source, &file)
            } else {
                Vec::new()
            }
        }
        "js" | "mjs" | "cjs" | "jsx" => express(source, &file),
        "ts" | "tsx" => {
            if source.contains("@Controller") {
                nest(source, &file)
            } else if source.contains("component:") || source.contains("loadChildren") {
                angular(source, &file)
            } else {
                express(source, &file)
            }
        }
        "java" => {
            if source.contains("javax.ws.rs")
                || source.contains("jakarta.ws.rs")
                || RE_JAXRS.is_match(source)
            {
                quarkus(source, &file)
            } else {
                spring(source, &file)
            }
        }
        "kt" => spring(source, &file),
        _ => Vec::new(),
    }
}

fn line_of(source: &str, byte: usize) -> u32 {
    source[..byte.min(source.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32
        + 1
}

/// Find the first `def <name>` / method name after `from` (Python handler).
fn py_handler_after(source: &str, from: usize) -> String {
    RE_PY_DEF
        .captures(&source[from.min(source.len())..])
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

fn symbol(class: &str, method: &str) -> String {
    match (class.is_empty(), method.is_empty()) {
        (false, false) => format!("{class}.{method}"),
        (true, false) => method.to_string(),
        _ => String::new(),
    }
}

// --- FastAPI ---
static RE_FASTAPI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"@(?:app|router)\.(get|post|put|delete|patch|options|head)\s*\(\s*["']([^"']*)["']"#,
    )
    .unwrap()
});

fn fastapi(source: &str, file: &str) -> Vec<Route> {
    RE_FASTAPI
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            let method = c[1].to_uppercase();
            let path = c[2].to_string();
            let handler = py_handler_after(source, m.end());
            Route {
                framework: "fastapi".into(),
                http_method: method,
                path,
                handler_class: String::new(),
                handler_method: handler.clone(),
                handler_symbol: symbol("", &handler),
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

// --- Flask ---
static RE_FLASK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@\w+\.route\s*\(\s*["']([^"']*)["']([^)]*)\)"#).unwrap());
static RE_METHODS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"methods\s*=\s*\[([^\]]*)\]"#).unwrap());
static RE_QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"["']([^"']+)["']"#).unwrap());

fn flask(source: &str, file: &str) -> Vec<Route> {
    let mut out = Vec::new();
    for c in RE_FLASK.captures_iter(source) {
        let m = c.get(0).unwrap();
        let path = c[1].to_string();
        let methods: Vec<String> = RE_METHODS
            .captures(&c[2])
            .map(|mc| {
                RE_QUOTED
                    .captures_iter(&mc[1])
                    .map(|q| q[1].to_uppercase())
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| vec!["GET".to_string()]);
        let handler = py_handler_after(source, m.end());
        let line = line_of(source, m.start());
        for method in methods {
            out.push(Route {
                framework: "flask".into(),
                http_method: method,
                path: path.clone(),
                handler_class: String::new(),
                handler_method: handler.clone(),
                handler_symbol: symbol("", &handler),
                file: file.to_string(),
                line,
            });
        }
    }
    out
}

// --- Express ---
static RE_EXPRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(?:app|router)\.(get|post|put|delete|patch|all)\s*\(\s*["'`]([^"'`]*)["'`]"#)
        .unwrap()
});

fn express(source: &str, file: &str) -> Vec<Route> {
    RE_EXPRESS
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            Route {
                framework: "express".into(),
                http_method: c[1].to_uppercase(),
                path: c[2].to_string(),
                handler_class: String::new(),
                handler_method: String::new(),
                handler_symbol: String::new(),
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

// --- NestJS ---
static RE_CONTROLLER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@Controller\s*\(\s*["']([^"']*)["']"#).unwrap());
static RE_NEST_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"@(Get|Post|Put|Delete|Patch)\s*\(\s*(?:["']([^"']*)["'])?\s*\)"#).unwrap()
});

fn nest(source: &str, file: &str) -> Vec<Route> {
    let prefix = RE_CONTROLLER
        .captures(source)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    RE_NEST_METHOD
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            let sub = c.get(2).map(|x| x.as_str()).unwrap_or("");
            Route {
                framework: "nestjs".into(),
                http_method: c[1].to_uppercase(),
                path: join_path(&prefix, sub),
                handler_class: String::new(),
                handler_method: String::new(),
                handler_symbol: String::new(),
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

// --- Spring ---
static RE_SPRING_CLASS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"@RequestMapping\s*\(\s*(?:value\s*=\s*|path\s*=\s*)?["']([^"']*)["']"#).unwrap()
});
static RE_SPRING_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"@(Get|Post|Put|Delete|Patch)Mapping\s*\(\s*(?:value\s*=\s*|path\s*=\s*)?["']([^"']*)["']"#,
    )
    .unwrap()
});

fn spring(source: &str, file: &str) -> Vec<Route> {
    // Class-level prefix: first @RequestMapping not immediately followed by a method annotation.
    let prefix = RE_SPRING_CLASS
        .captures(source)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    RE_SPRING_METHOD
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            Route {
                framework: "spring".into(),
                http_method: c[1].to_uppercase(),
                path: join_path(&prefix, &c[2]),
                handler_class: String::new(),
                handler_method: String::new(),
                handler_symbol: String::new(),
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

// --- Quarkus (JAX-RS) ---
static RE_JAXRS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@(GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS)\b"#).unwrap());
static RE_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@Path\s*\(\s*["']([^"']*)["']"#).unwrap());

fn quarkus(source: &str, file: &str) -> Vec<Route> {
    let paths: Vec<_> = RE_PATH.captures_iter(source).collect();
    // The first @Path is treated as the class-level prefix.
    let class_prefix = paths.first().map(|c| c[1].to_string()).unwrap_or_default();
    RE_JAXRS
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            // A method-level @Path following the HTTP annotation (within ~200 bytes).
            let window = &source[m.end()..(m.end() + 200).min(source.len())];
            let sub = RE_PATH
                .captures(window)
                .map(|p| p[1].to_string())
                .unwrap_or_default();
            Route {
                framework: "quarkus".into(),
                http_method: c[1].to_uppercase(),
                path: join_path(&class_prefix, &sub),
                handler_class: String::new(),
                handler_method: String::new(),
                handler_symbol: String::new(),
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

// --- Angular router config ---
static RE_NG_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"path\s*:\s*["']([^"']*)["']"#).unwrap());

fn angular(source: &str, file: &str) -> Vec<Route> {
    RE_NG_PATH
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            Route {
                framework: "angular".into(),
                http_method: String::new(),
                path: c[1].to_string(),
                handler_class: String::new(),
                handler_method: String::new(),
                handler_symbol: String::new(),
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

static RE_PY_DEF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:async\s+)?def\s+(\w+)\s*\("#).unwrap());

/// Join a class-level prefix and a method sub-path into a single leading-slash path.
fn join_path(prefix: &str, sub: &str) -> String {
    let mut parts = Vec::new();
    let p = prefix.trim_matches('/');
    let s = sub.trim_matches('/');
    if !p.is_empty() {
        parts.push(p);
    }
    if !s.is_empty() {
        parts.push(s);
    }
    format!("/{}", parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routes(src: &str, name: &str) -> Vec<Route> {
        extract_routes(src, Path::new(name))
    }

    #[test]
    fn fastapi_routes() {
        let src = "\
from fastapi import APIRouter
router = APIRouter()

@router.get(\"/users\")
async def list_users():
    return []

@router.post(\"/users\")
def create_user():
    return {}
";
        let r = routes(src, "api.py");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].framework, "fastapi");
        assert_eq!(r[0].http_method, "GET");
        assert_eq!(r[0].path, "/users");
        assert_eq!(r[0].handler_method, "list_users");
        assert_eq!(r[1].http_method, "POST");
    }

    #[test]
    fn flask_routes_with_methods() {
        let src = "\
from flask import Flask
app = Flask(__name__)

@app.route(\"/login\", methods=[\"GET\", \"POST\"])
def login():
    return \"ok\"
";
        let r = routes(src, "app.py");
        assert_eq!(r.len(), 2);
        assert!(r
            .iter()
            .all(|x| x.path == "/login" && x.framework == "flask"));
        assert!(r.iter().any(|x| x.http_method == "GET"));
        assert!(r.iter().any(|x| x.http_method == "POST"));
        assert_eq!(r[0].handler_method, "login");
    }

    #[test]
    fn express_routes() {
        let src = "const router = require('express').Router();\nrouter.get('/health', (req, res) => res.send('ok'));\napp.post('/items', handler);\n";
        let r = routes(src, "routes.js");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].http_method, "GET");
        assert_eq!(r[0].path, "/health");
        assert_eq!(r[1].http_method, "POST");
    }

    #[test]
    fn spring_routes_with_prefix() {
        let src = "\
@RestController
@RequestMapping(\"/api\")
public class UserController {
    @GetMapping(\"/users\")
    public List<User> list() { return null; }
    @PostMapping(\"/users\")
    public User create() { return null; }
}
";
        let r = routes(src, "UserController.java");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].framework, "spring");
        assert_eq!(r[0].path, "/api/users");
        assert_eq!(r[0].http_method, "GET");
    }

    #[test]
    fn quarkus_jaxrs_routes() {
        let src = "\
import jakarta.ws.rs.GET;
@Path(\"/greet\")
public class GreetResource {
    @GET
    @Path(\"/hello\")
    public String hello() { return \"hi\"; }
}
";
        let r = routes(src, "GreetResource.java");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].framework, "quarkus");
        assert_eq!(r[0].http_method, "GET");
        assert_eq!(r[0].path, "/greet/hello");
    }

    #[test]
    fn nest_routes_with_controller_prefix() {
        let src = "\
@Controller('cats')
export class CatsController {
    @Get()
    findAll() {}
    @Post('/create')
    create() {}
}
";
        let r = routes(src, "cats.controller.ts");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].framework, "nestjs");
        assert_eq!(r[0].path, "/cats");
        assert_eq!(r[1].path, "/cats/create");
    }

    #[test]
    fn angular_routes() {
        let src = "const routes: Routes = [\n  { path: 'home', component: HomeComponent },\n  { path: 'about', loadChildren: () => x },\n];\n";
        let r = routes(src, "app-routing.module.ts");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].framework, "angular");
        assert_eq!(r[0].path, "home");
    }
}
