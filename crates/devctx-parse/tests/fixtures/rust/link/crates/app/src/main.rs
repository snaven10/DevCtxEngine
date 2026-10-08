use demo_store::ops::Named;
use demo_store::{store::Store as S2, Store};
use mystery::Thing;

mod util;
use util::{self, tidy};

#[tokio::main]
async fn main() {
    let s = Store::open("p").unwrap();
    let t = Store::new();
    t.save();
    s.get("k");
    let n = t.name();
    let v: serde_json::Value = serde_json::from_str("{}").unwrap();
    tokio::spawn(async {});
    println!("{} {:?} {}", format!("{}", 1), v, n);
    let w = vec![1];
    w.len();
    util::tidy();
    tidy();
    helper_free();
    let items = vec![1u32];
    items.iter().map(|it| it.count_ones());
    demo_store::helpers::make().run();
    Thing::go();
    let _ = S2::new();
}

fn helper_free() {}

fn opener() -> Result<(), String> {
    let st = Store::open("x")?;
    st.save();
    Ok(())
}

struct Wrapper {
    inner: Store,
}

impl Wrapper {
    fn go(&self, store: &Store) {
        self.inner.save();
        store.save();
        if let Some(inner) = None::<Store> {
            inner.save();
        }
        let f = |store| store.save();
        match Some(1) {
            Some(store) => store.save(),
            None => false,
        };
        f(1);
    }
}
