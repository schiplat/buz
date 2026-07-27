# Buz Docs (Hugo)

Local preview:

```bash
cd docs
hugo server --port 3001
# → http://127.0.0.1:3001/
```

Initial theme setup:

```bash
git submodule update --init docs/themes/hextra
```

Build for production:

```bash
cd docs
hugo --minify
# → docs/public/
```

## Sections

| Section | Path | Content |
|---------|------|---------|
| Architecture | `content/docs/01-architecture/` | B-U-Z principles, tech stack, pipeline format, API reference |
| Development | `content/docs/02-development/` | Project structure, database schema, implementation guides |
| Operations | `content/docs/03-operations/` | Deployment, configuration, runbooks |
