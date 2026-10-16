//! Java (PLAN-009 TASK-005, DD-7): what a file declares about itself, the
//! type a `var` takes from its initializer, and how a type name, a declared
//! type and an arity are read for the link pass.

use tree_sitter::Node;

use super::scope::{TypeText, Via};
use super::LangResolver;
use crate::facts::ImportFact;

/// Java.
pub struct Java;

impl LangResolver for Java {
    fn package_from_path(&self, _path: &str) -> Option<String> {
        None // `package x.y;` in the source
    }

    /// `public`, or a member of an interface or annotation (implicitly
    /// public unless `private`).
    fn exported(&self, def: Node<'_>, _name: &str, bytes: &[u8]) -> Option<bool> {
        let modifiers = {
            let mut cursor = def.walk();
            let found = def
                .named_children(&mut cursor)
                .find(|c| c.kind() == "modifiers")
                .and_then(|m| m.utf8_text(bytes).ok())
                .unwrap_or_default();
            found
        };
        let has = |word: &str| modifiers.split_whitespace().any(|w| w == word);
        if has("public") {
            return Some(true);
        }
        if has("private") {
            return Some(false);
        }
        let in_interface = def
            .parent()
            .and_then(|body| body.parent())
            .is_some_and(|c| {
                matches!(
                    c.kind(),
                    "interface_declaration" | "annotation_type_declaration"
                )
            });
        Some(in_interface)
    }

    fn import_target(&self, imp: &ImportFact) -> String {
        if imp.wildcard {
            format!("{}.*", imp.path)
        } else {
            imp.path.clone()
        }
    }

    /// `new T(…)` and `(T) e` are `T`; `T.of(…)` (a capitalised receiver)
    /// is `T` only as a guess the link pass confirms ([`Via::Static`]).
    fn init_type(&self, init: Node<'_>, bytes: &[u8]) -> Option<(TypeText, Via)> {
        let text = |n: Node<'_>| n.utf8_text(bytes).ok().map(str::to_string);
        match init.kind() {
            "object_creation_expression" | "cast_expression" => {
                let ty = text(init.child_by_field_name("type")?)?;
                Some((TypeText::parse(&ty)?, Via::Local))
            }
            "parenthesized_expression" => self.init_type(init.named_child(0)?, bytes),
            "method_invocation" => {
                let object = init.child_by_field_name("object")?;
                let name = text(object)?;
                let typeish = object.kind() == "identifier"
                    && name.chars().next().is_some_and(char::is_uppercase)
                    && !is_constant_name(&name);
                if !typeish {
                    return None;
                }
                Some((TypeText::parse(&name)?, Via::Static))
            }
            _ => None,
        }
    }
}

/// An all-capitals name (`LOG`, `MAX_SIZE`): a constant, never a type
/// (DD-7). A single capital (`T`) is a type parameter's, not a constant's.
pub fn is_constant_name(name: &str) -> bool {
    name.chars().count() > 1
        && name.chars().any(char::is_alphabetic)
        && name
            .chars()
            .all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The type a Java declaration's signature gives `name`: a method's return
/// type (`public static Uni<Office> find(String c)` → `Uni`) or a field's
/// type (`private static final Logger LOG = …` → `Logger`), generic
/// arguments and array brackets dropped. `None` for `void`, a constructor or
/// a signature that does not name `name`.
pub fn declared_type(signature: &str, name: &str) -> Option<String> {
    let head = before_name(signature, name)?;
    // Generic arguments out first: `Map<String, List<X>>` holds spaces.
    let mut flat = String::with_capacity(head.len());
    let mut depth = 0usize;
    for c in head.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => flat.push(c),
            _ => {}
        }
    }
    let last = flat.split_whitespace().last()?;
    let ty = last.trim_end_matches("[]").trim_end_matches("...");
    const NOT_TYPES: &[&str] = &[
        "void",
        "public",
        "private",
        "protected",
        "static",
        "final",
        "abstract",
        "synchronized",
        "native",
        "default",
        "transient",
        "volatile",
        "strictfp",
    ];
    let ok = !ty.is_empty()
        && !NOT_TYPES.contains(&ty)
        && !ty.starts_with('@')
        && ty
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.');
    ok.then(|| ty.to_string())
}

/// The text of `signature` before the declaration of `name` (a word
/// followed by `(`, `=`, `;`, `,` or the end).
fn before_name<'s>(signature: &'s str, name: &str) -> Option<&'s str> {
    let mut from = 0;
    while let Some(i) = signature[from..].find(name) {
        let at = from + i;
        let end = at + name.len();
        let before_ok = signature[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '$'));
        let after = signature[end..].trim_start();
        let after_ok = after.is_empty() || after.starts_with(['(', '=', ';', ',']);
        if before_ok && after_ok && at > 0 {
            return Some(&signature[..at]);
        }
        from = end;
    }
    None
}

/// How many arguments a Java callable's signature takes, and whether its
/// last parameter is varargs: `f(Long id, List<String> names)` → `(2,
/// false)`. `None` when the signature has no parameter list.
pub fn arity(signature: &str) -> Option<(usize, bool)> {
    let open = signature.find('(')?;
    let mut depth = 0usize;
    let mut count = 0usize;
    let mut any = false;
    let mut end = None;
    for (i, c) in signature[open + 1..].char_indices() {
        match c {
            '(' | '<' => depth += 1,
            ')' if depth == 0 => {
                end = Some(open + 1 + i);
                break;
            }
            ')' | '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => count += 1,
            c if !c.is_whitespace() => any = true,
            _ => {}
        }
    }
    let params = &signature[open + 1..end?];
    let n = if any { count + 1 } else { 0 };
    Some((n, params.contains("...")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_types_from_signatures() {
        assert_eq!(
            declared_type(
                "public static Uni<Office> findByCode(String c)",
                "findByCode"
            )
            .as_deref(),
            Some("Uni")
        );
        assert_eq!(
            declared_type(
                "private static final Logger LOG = Logger.getLogger(\"x\");",
                "LOG"
            )
            .as_deref(),
            Some("Logger")
        );
        assert_eq!(
            declared_type("Map<String, List<Long>> index;", "index").as_deref(),
            Some("Map")
        );
        assert_eq!(declared_type("public void run()", "run"), None);
        assert_eq!(
            declared_type("public OrderService(Repo r)", "OrderService"),
            None
        );
        assert_eq!(
            declared_type("@Inject OrderRepository repository;", "repository").as_deref(),
            Some("OrderRepository")
        );
    }

    #[test]
    fn arities_from_signatures() {
        assert_eq!(arity("void f()"), Some((0, false)));
        assert_eq!(
            arity("void f(Long id, Map<String, Long> m)"),
            Some((2, false))
        );
        assert_eq!(arity("void f(String... xs)"), Some((1, true)));
        assert_eq!(arity("int x;"), None);
    }

    #[test]
    fn constants_are_not_types() {
        assert!(is_constant_name("LOG"));
        assert!(is_constant_name("MAX_SIZE"));
        assert!(!is_constant_name("T"));
        assert!(!is_constant_name("Office"));
        // Spelt like a constant: the parse does not take it for a type, the
        // link pass still finds `java.util.UUID` through the import.
        assert!(is_constant_name("UUID"));
    }
}
