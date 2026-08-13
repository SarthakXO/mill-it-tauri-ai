# Mill-it Ops

Local Tauri + React analytics assistant backed by read-only PostgreSQL and a local Ollama model.

## Environment

Configure the backend in `.env`:

```env
DATABASE_URL=postgresql://...
OLLAMA_BASE_URL=http://localhost:11434
OLLAMA_MODEL=qwen3.5:9b
```

Ollama configuration is owned centrally by the Rust `OllamaConfig` singleton and is never exposed to React.

## Analytics architecture

Supported common analytics compile from a typed `AnalyticsPlan` into parameterized SQL in Rust. Specialized customer, reorder, forecast, and product-performance algorithms remain deterministic tools. Unsupported read-only analytics may use the guarded generated-SQL fallback. Every database query still passes the AST safety validator and executes in a read-only transaction.
