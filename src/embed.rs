use model2vec_rs::model::StaticModel;

use crate::error::{LegionError, Result};

const MODEL_NAME: &str = "minishlab/potion-base-8M";

/// Wrapper around the model2vec StaticModel for computing text embeddings.
pub struct EmbedModel {
    model: StaticModel,
}

impl EmbedModel {
    /// Load the embedding model from the Hugging Face cache.
    ///
    /// Downloads the model on first use (~8MB). Subsequent loads use the cache.
    pub fn load() -> Result<Self> {
        let model = StaticModel::from_pretrained(MODEL_NAME, None, None, None).map_err(|e| {
            LegionError::Embedding(format!("failed to load embedding model {MODEL_NAME}: {e}"))
        })?;
        Ok(Self { model })
    }

    /// Compute an embedding for a single text.
    ///
    /// Returns a Vec<f32> of dimension 256 (for potion-base-8M).
    pub fn encode_one(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.model.encode_single(text))
    }

    /// Compute embeddings for multiple texts.
    #[allow(dead_code)]
    pub fn encode_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(self.model.encode(texts))
    }
}

/// Compute cosine similarity between two embedding vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot: f32 = 0.0;
    let mut norm_a: f32 = 0.0;
    let mut norm_b: f32 = 0.0;

    for (&ai, &bi) in a.iter().zip(b.iter()) {
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }

    let norm_a = norm_a.sqrt();
    let norm_b = norm_b.sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

/// Serialize an embedding to bytes for SQLite BLOB storage.
pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for &val in embedding {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Deserialize an embedding from SQLite BLOB bytes.
pub fn embedding_from_bytes(bytes: &[u8]) -> Vec<f32> {
    // as_chunks::<4>() yields &[u8; 4] directly (its .1 remainder of any
    // trailing bytes is ignored, matching the old chunks_exact(4)).
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&chunk| f32::from_le_bytes(chunk))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_identical_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_empty_vectors() {
        let sim = cosine_similarity(&[], &[]);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn embedding_roundtrip_bytes() {
        let original = vec![1.0f32, -2.5, std::f32::consts::PI, 0.0, f32::MAX, f32::MIN];
        let bytes = embedding_to_bytes(&original);
        let restored = embedding_from_bytes(&bytes);
        assert_eq!(original, restored);
    }

    #[test]
    fn embedding_to_bytes_size() {
        let embedding = vec![0.0f32; 256];
        let bytes = embedding_to_bytes(&embedding);
        assert_eq!(bytes.len(), 256 * 4);
    }

    #[test]
    fn embedding_from_bytes_empty() {
        let result = embedding_from_bytes(&[]);
        assert!(result.is_empty());
    }

    // Model loading and encoding tests require the model to be downloaded.
    // Run with: cargo test -- --ignored embed
    #[test]
    #[ignore]
    fn load_model_and_encode() {
        let model = EmbedModel::load().expect("load model");
        let embedding = model.encode_one("hello world").expect("encode");
        assert_eq!(embedding.len(), 256);

        // Verify it's normalized (unit length)
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.1,
            "expected ~unit norm, got {}",
            norm
        );
    }

    #[test]
    #[ignore]
    fn similar_texts_have_higher_cosine() {
        let model = EmbedModel::load().expect("load model");
        let e1 = model
            .encode_one("rust ownership and borrowing")
            .expect("e1");
        let e2 = model.encode_one("rust borrow checker rules").expect("e2");
        let e3 = model.encode_one("chocolate cake recipe").expect("e3");

        let sim_related = cosine_similarity(&e1, &e2);
        let sim_unrelated = cosine_similarity(&e1, &e3);

        assert!(
            sim_related > sim_unrelated,
            "related: {}, unrelated: {}",
            sim_related,
            sim_unrelated
        );
    }
}
