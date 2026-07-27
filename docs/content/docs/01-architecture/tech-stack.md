---
title: Tech Stack
weight: 2
description: "Technology choices for the Buz platform"
---

## Backend

| Domain | Technology | Purpose |
|--------|-----------|---------|
| Web framework | axum + tokio | High-performance async REST, WebSocket, long polling |
| Database | sqlx + SQLite | Async data access, compile-time SQL verification |
| HTTP client | reqwest | Long polling, log upload, Vault communication |
| SSH execution | russh | Pure Rust async SSH, no C dependencies |
| Local execution | std::process | Lightweight native LocalShell execution |
| Template rendering | minijinja | Command template and variable rendering |
| Secrets | vaultrs | Async Vault credential retrieval |
| Log masking | aho-corasick | High-performance multi-pattern secret redaction |
| Static embedding | rust-embed | Frontend assets embedded into single binary |
| Metrics | metrics + prometheus-exporter | Runtime metrics exposure |

## Frontend

| Domain | Technology |
|--------|-----------|
| Framework | Vue 3 (`<script setup lang="ts">`) |
| Language | TypeScript |
| Styling | TailwindCSS |
| Components | shadcn/ui (copy-based, zero global dependency) |
| Terminal | Xterm.js (virtual scroll, chunked rendering) |
| Build | Vite |
| State | Pinia |
| Routing | Vue Router (Hash mode) |
