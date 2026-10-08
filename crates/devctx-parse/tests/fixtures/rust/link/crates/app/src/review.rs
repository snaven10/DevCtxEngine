use std::sync::Arc;

use demo_store::failure::Failure;
use demo_store::helpers::make;
use demo_store::ops::Named;
use demo_store::Store;

fn polish() {}

/// A crate the manifest declares from the registry, though the workspace
/// has one of that name; a renamed path dependency; one inherited from the
/// workspace; a workspace crate nobody declares.
pub fn crates() {
    utils::tidy_up();
    store2::helpers::make();
    wsdep::Store::new();
    orphan::lone();
}

/// Several bounds: the one that has the method.
pub fn first<T: Clone + Named>(x: T) -> String {
    x.name()
}

pub fn second<T>(x: T) -> String
where
    T: Send + Named,
{
    x.name()
}

pub fn third(x: Arc<dyn Send + Named>) -> String {
    x.name()
}

/// Two `From` impls: no surer than medium; an inherent method over a
/// trait's of the same name.
pub fn overloads(f: Failure) -> String {
    let _ = Failure::from(String::new());
    f.name()
}

/// Two definitions of one name under `#[cfg]`.
pub fn platform() -> u8 {
    demo_store::helpers::platform()
}

/// A `use` inside a function is that function's.
pub fn t1() {
    use crate::util::polish;
    polish();
}

pub fn t2() {
    use crate::extra::polish;
    polish();
}

/// A missing method of a type with an external supertype: medium.
pub fn missing(t: Store) {
    t.render();
}

/// `impl Ext for serde_json::Value` is no `impl` of the repository's
/// `Value`.
pub fn not_mine(v: demo_store::helpers::Value) {
    v.ext();
}

/// `Arc::new(Store::open(…)?)` holds a `Store`; a nested `make` shadows the
/// imported one.
pub fn values() -> Result<(), String> {
    let shared = Arc::new(Store::open("p")?);
    shared.save();
    fn make() -> Store {
        Store::new()
    }
    let x = make();
    x.save();
    Ok(())
}

/// An associated function with a capital on an enum is no variant.
pub fn modes() {
    demo_store::helpers::Mode::Parse();
    format!("{}", polish());
}

/// `T::default()` that no `impl` defines (a derive) is a `T`.
pub fn defaults() {
    let st = Store::default();
    st.save();
}

/// `let x = a.f()?`: a call on `x` follows `f`'s declared return type.
pub fn chained(t: Store) -> Result<(), String> {
    let l = t.logger();
    l.info();
    let opened = Store::open("x")?;
    let again = opened.logger();
    again.info();
    Ok(())
}

/// A type nothing resolves types nothing: never a guess by name.
pub fn mystery_typed(t: mystery::Thing) {
    t.tidy_up();
}

/// A `use` inside a closure is the closure's.
pub fn closure_use() {
    let c = || {
        use crate::extra::polish;
        polish();
    };
    c();
    polish();
}
