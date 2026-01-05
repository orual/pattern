#[cfg(test)]
mod embeddings_tests {
    use pattern_core::embeddings::{EmbeddingConfig, create_provider};

    #[tokio::test]
    #[cfg(feature = "embed-cloud")]
    #[ignore = "requires OPENAI_API_KEY environment variable set"]
    async fn test_openai_embeddings() {
        // Skip if no API key
        let api_key = match std::env::var("OPENAI_API_KEY") {
            Ok(key) => key,
            Err(_) => {
                panic!("OPENAI_API_KEY not set");
            }
        };

        let config = EmbeddingConfig::OpenAI {
            model: "text-embedding-3-small".to_string(),
            api_key,
            dimensions: Some(256), // Use smaller dimensions for testing
        };

        let provider = create_provider(config).await.unwrap();

        // Test single embedding
        let embedding = provider.embed("Hello, world!").await.unwrap();
        assert_eq!(embedding.dimensions, 256);
        assert_eq!(embedding.vector.len(), 256);
        assert_eq!(embedding.model, "text-embedding-3-small");

        // Test batch embedding
        let texts = vec![
            "First text".to_string(),
            "Second text".to_string(),
            "Third text".to_string(),
        ];
        let embeddings = provider.embed_batch(&texts).await.unwrap();
        assert_eq!(embeddings.len(), 3);
        for embedding in embeddings {
            assert_eq!(embedding.dimensions, 256);
        }
    }

    #[tokio::test]
    #[cfg(feature = "embed-cloud")]
    #[ignore = "requires COHERE_API_KEY environment variable set"]
    async fn test_cohere_embeddings() {
        // Skip if no API key
        let api_key = match std::env::var("COHERE_API_KEY") {
            Ok(key) => key,
            Err(_) => {
                panic!("COHERE_API_KEY not set");
            }
        };

        let config = EmbeddingConfig::Cohere {
            model: "embed-english-light-v3.0".to_string(),
            api_key,
            input_type: Some("search_document".to_string()),
        };

        let provider = create_provider(config).await.unwrap();

        // Test single embedding
        let embedding = provider.embed("Hello, world!").await.unwrap();
        assert_eq!(embedding.dimensions, 384);
        assert_eq!(embedding.vector.len(), 384);
        assert_eq!(embedding.model, "embed-english-light-v3.0");
    }

    #[tokio::test]
    #[cfg(feature = "embed-ollama")]
    async fn test_ollama_embeddings() {
        let config = EmbeddingConfig::Ollama {
            model: "all-minilm".to_string(),
            url: "http://localhost:11434".to_string(),
        };

        let provider = match create_provider(config).await {
            Ok(p) => p,
            Err(e) => {
                panic!("Failed to create provider: {:?}", e);
            }
        };

        // Check health first
        if provider.health_check().await.is_err() {
            eprintln!("Skipping Ollama test - Ollama not healthy");
            return;
        }

        // Test single embedding
        let embedding = provider.embed("Hello, world!").await.unwrap();
        assert_eq!(embedding.dimensions, 384);
        assert_eq!(embedding.vector.len(), 384);
        assert_eq!(embedding.model, "all-minilm");
    }

    #[tokio::test]
    #[cfg(feature = "embed-candle")]
    async fn test_candle_embeddings() {
        // This test requires downloading model files, so we'll use a small model
        let config = EmbeddingConfig::Candle {
            model: "jinaai/jina-embeddings-v2-small-en".to_string(),
            cache_dir: Some("./test_cache".to_string()),
        };

        // Skip this test in CI or if we can't download models
        if std::env::var("CI").is_ok() {
            eprintln!("Skipping Candle test in CI environment");
            return;
        }

        let provider = match create_provider(config).await {
            Ok(p) => p,
            Err(e) => {
                panic!("Failed to load model: {}", e);
            }
        };

        // Test single embedding
        let embedding = provider.embed("Hello, world!").await.unwrap();
        assert_eq!(embedding.dimensions, 512);
        assert_eq!(embedding.vector.len(), 512);

        // Test batch embedding
        let texts = vec!["First text".to_string(), "Second text".to_string()];
        let embeddings = provider.embed_batch(&texts).await.unwrap();
        assert_eq!(embeddings.len(), 2);

        // Clean up test cache
        let _ = std::fs::remove_dir_all("./test_cache");
    }

    #[tokio::test]
    async fn test_embedding_similarity() {
        // Use any available provider for this test
        let config = if std::env::var("OPENAI_API_KEY").is_ok() {
            EmbeddingConfig::OpenAI {
                model: "text-embedding-3-small".to_string(),
                api_key: std::env::var("OPENAI_API_KEY").unwrap(),
                dimensions: Some(256),
            }
        } else {
            // Fall back to candle for local testing
            #[cfg(feature = "embed-candle")]
            {
                EmbeddingConfig::Candle {
                    model: "jinaai/jina-embeddings-v2-small-en".to_string(),
                    cache_dir: None,
                }
            }
            #[cfg(not(feature = "embed-candle"))]
            {
                eprintln!(
                    "Skipping test: no embedding provider available (set OPENAI_API_KEY or enable embed-candle feature)"
                );
                return;
            }
        };

        let provider = match create_provider(config).await {
            Ok(p) => p,
            Err(e) => {
                panic!("Failed to create provider: {:?}", e);
            }
        };

        // Test that similar texts have high similarity
        let texts = vec![
            "The cat sat on the mat".to_string(),
            "A cat was sitting on a mat".to_string(),
            "Python is a programming language".to_string(),
        ];

        let embeddings = provider.embed_batch(&texts).await.unwrap();

        // Cat sentences should be more similar to each other than to the Python sentence
        let sim_cats = embeddings[0].cosine_similarity(&embeddings[1]).unwrap();
        let sim_cat_python = embeddings[0].cosine_similarity(&embeddings[2]).unwrap();

        assert!(
            sim_cats > sim_cat_python,
            "Similar sentences should have higher similarity: {} vs {}",
            sim_cats,
            sim_cat_python
        );
    }

    #[tokio::test]
    #[ignore = "requires OpenAI API key, export OPENAI_API_KEY with a valid key to run"]
    async fn test_empty_input_error() {
        // Try to create a provider, but skip test if it fails
        let config = if std::env::var("OPENAI_API_KEY").is_ok() {
            EmbeddingConfig::OpenAI {
                model: "text-embedding-3-small".to_string(),
                api_key: std::env::var("OPENAI_API_KEY").unwrap(),
                dimensions: Some(256),
            }
        } else {
            panic!("set OPENAI_API_KEY and re-run")
        };

        let provider = match create_provider(config).await {
            Ok(p) => p,
            Err(e) => {
                panic!("Failed to create provider {}", e);
            }
        };

        // Empty string should error
        assert!(provider.embed("").await.is_err());
        assert!(provider.embed("   ").await.is_err());

        // Empty batch should error
        assert!(provider.embed_batch(&[]).await.is_err());
        assert!(provider.embed_batch(&["".to_string()]).await.is_err());
    }
}
