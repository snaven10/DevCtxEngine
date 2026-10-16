//! The embedding provider abstraction shared by local and API backends.

use std::sync::{Arc, Mutex};

use crate::error::Result;

/// A source of text embeddings. All local providers L2-normalize their output
/// so cosine similarity is consistent across backends.
pub trait EmbeddingProvider: Send + Sync {
    /// Embed a batch of documents. Output length equals `texts.len()`, and each
    /// vector has length [`dimension`](Self::dimension).
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Embed a single query string.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed(std::slice::from_ref(&text.to_string()))?;
        Ok(out.pop().unwrap_or_default())
    }

    /// Output vector dimension.
    fn dimension(&self) -> usize;

    /// Human-readable model identifier.
    fn model_name(&self) -> &str;
}

/// How a [`LazyEmbedder`] builds the real provider.
type Loader<'a> = Box<dyn Fn() -> Result<Arc<dyn EmbeddingProvider>> + Send + Sync + 'a>;

/// An embedder that is only built when something is actually embedded.
///
/// `dimension` and `model_name` are known from the configuration, so a caller
/// that only needs them (the dimension check of an indexing run, the model
/// recorded in `index_state`) never pays the model load. A reindex that finds
/// every vector reusable never calls [`embed`](EmbeddingProvider::embed), and
/// with it the ~700 MiB load transient is skipped.
pub struct LazyEmbedder<'a> {
    dimension: usize,
    name: String,
    loader: Loader<'a>,
    loaded: Mutex<Option<Arc<dyn EmbeddingProvider>>>,
}

impl<'a> LazyEmbedder<'a> {
    /// `dimension` and `name` must be those of what `loader` builds.
    pub fn new(
        dimension: usize,
        name: impl Into<String>,
        loader: impl Fn() -> Result<Arc<dyn EmbeddingProvider>> + Send + Sync + 'a,
    ) -> Self {
        Self {
            dimension,
            name: name.into(),
            loader: Box::new(loader),
            loaded: Mutex::new(None),
        }
    }

    /// Whether the real provider has been built yet.
    pub fn is_loaded(&self) -> bool {
        self.loaded.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    fn get(&self) -> Result<Arc<dyn EmbeddingProvider>> {
        let mut g = self.loaded.lock().map_err(|_| {
            crate::error::EmbedError::BadResponse("the lazy embedder lock is poisoned".into())
        })?;
        if let Some(e) = g.as_ref() {
            return Ok(e.clone());
        }
        let e = (self.loader)()?;
        *g = Some(e.clone());
        Ok(e)
    }
}

impl EmbeddingProvider for LazyEmbedder<'_> {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.get()?.embed(texts)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.get()?.embed_query(text)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn model_name(&self) -> &str {
        &self.name
    }
}

/// L2-normalize a vector in place. Zero vectors are left unchanged.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_to_unit_length() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        let norm = (v[0] * v[0] + v[1] * v[1]).sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn zero_vector_is_left_alone() {
        let mut v = vec![0.0, 0.0, 0.0];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }
}
