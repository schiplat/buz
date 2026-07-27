---
title: API Reference
weight: 4
description: "Buz Manager REST API reference"
---

All API endpoints use the prefix `/api/v1/`.

## Response Format

```json
{
  "code": 0,
  "message": "success",
  "data": {}
}
```

### Error Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1001 | Authentication failed |
| 1002 | Token expired |
| 1003 | Insufficient permissions |
| 2001 | Resource not found |
| 2002 | Resource conflict |
| 3001 | Validation failed |
| 5000 | Internal server error |

## Authentication

| Method | Path | Auth |
|--------|------|------|
| POST | `/api/v1/auth/login` | None |

### POST /auth/login

Request:
```json
{ "username": "admin", "password": "admin" }
```

Response:
```json
{ "code": 0, "data": { "token": "eyJ..." } }
```

## Runners

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/runners` | Admin JWT |
| GET | `/api/v1/runners/:id` | Admin JWT |
| POST | `/api/v1/runners` | Admin JWT |
| DELETE | `/api/v1/runners/:id` | Admin JWT |
| POST | `/api/v1/runners/register` | Register Token |
| POST | `/api/v1/runners/heartbeat` | Runner JWT |
| GET | `/api/v1/runners/poll` | Runner JWT |

### POST /runners — Create Runner (Admin)

Request:
```json
{
  "name": "prod-node-01",
  "labels": ["linux-x64", "prod"],
  "version": "0.1.0"
}
```

Response:
```json
{
  "code": 0,
  "data": {
    "id": "uuid",
    "name": "prod-node-01",
    "register_token": "one-time-token"
  }
}
```

### POST /runners/register — Runner Registration

Request:
```json
{
  "name": "prod-node-01",
  "register_token": "one-time-token",
  "labels": ["linux-x64"],
  "version": "0.1.0"
}
```

Response:
```json
{
  "code": 0,
  "data": {
    "runner_id": "uuid",
    "token": "jwt"
  }
}
```

### POST /runners/heartbeat — Runner Heartbeat

Request (Bearer token):
```json
{ "status": "online" }
```

### GET /runners/poll — Runner Task Poll

Response (with task):
```json
{
  "code": 0,
  "data": {
    "job_id": "uuid",
    "pipeline_id": "uuid",
    "tasks": [
      { "id": "uuid", "step_order": 0, "name": "build", "exec_type": "local", "command_template": "npm run build", "work_dir": "" }
    ]
  }
}
```

Empty response: `204 No Content`

## Pipelines

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/pipelines` | Admin JWT |
| POST | `/api/v1/pipelines` | Admin JWT |
| GET | `/api/v1/pipelines/:id` | Admin JWT |
| PUT | `/api/v1/pipelines/:id` | Admin JWT |
| DELETE | `/api/v1/pipelines/:id` | Admin JWT |
| POST | `/api/v1/pipelines/:id/trigger` | Admin JWT |

### POST /pipelines — Create

Request:
```json
{
  "name": "hello-world",
  "description": "A simple test pipeline",
  "trigger_type": "manual",
  "runner_labels": ["linux-x64"],
  "timeout_seconds": 3600,
  "concurrency_group": null,
  "steps": [
    { "name": "echo-step", "run": "echo Hello from Buz" },
    { "name": "build", "run": "npm run build", "dir": "./frontend" }
  ]
}
```

### POST /pipelines/:id/trigger — Trigger Run

Response:
```json
{
  "code": 0,
  "data": { "job_id": "uuid" }
}
```

## Jobs

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/jobs` | JWT |
| GET | `/api/v1/jobs/:id` | JWT |
| POST | `/api/v1/jobs/:id/cancel` | JWT |
| POST | `/api/v1/jobs/:id/claim` | Runner Token |
| PATCH | `/api/v1/jobs/:id/status` | Runner Token |

## Logs

| Method | Path | Auth |
|--------|------|------|
| POST | `/api/v1/jobs/:id/logs` | Runner Token |
| GET | `/api/v1/jobs/:id/logs` | JWT |

## Hosts

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/hosts` | JWT |
| POST | `/api/v1/hosts` | JWT |
| PUT | `/api/v1/hosts/:id` | JWT |
| DELETE | `/api/v1/hosts/:id` | JWT |
