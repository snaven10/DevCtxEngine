//! The embedding provider abstraction shared by local and API backends.

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
