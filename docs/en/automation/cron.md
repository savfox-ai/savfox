# Cron Service

The gateway includes a built-in cron service for scheduling recurring tasks.

## Schedule Types

### At (one-time)
Run once at a specific time:
```json
{"type": "at", "time": "2026-02-14T09:00:00Z"}
```

### Every (interval)
Run at fixed intervals:
```json
{"type": "every", "interval_secs": 3600}
```

### Cron (expression)
Standard cron expressions:
```json
{"type": "cron", "expression": "0 */6 * * *"}
```

Cron format: `minute hour day-of-month month day-of-week`

## Payload Types

### System Event
Trigger an internal system event:
```json
{"type": "systemEvent", "event": "health_check"}
```

### Agent Turn
Start an agent conversation turn:
```json
{"type": "agentTurn", "prompt": "Check all service endpoints and report status"}
```

## Management via WS-RPC

### Create a cron job
```json
{"method": "cron.create", "params": {"name": "hourly-check", "schedule": {"type": "every", "interval_secs": 3600}, "payload": {"type": "agentTurn", "prompt": "Check status"}}, "id": 1}
```

### List jobs
```json
{"method": "cron.list", "params": {}, "id": 2}
```

### Delete a job
```json
{"method": "cron.delete", "params": {"id": "job-id"}, "id": 3}
```

### Manually run a job
```json
{"method": "cron.run", "params": {"id": "job-id"}, "id": 4}
```

## Error Handling

Failed cron runs use exponential backoff:
- First retry: 60 seconds
- Second retry: 120 seconds
- Maximum backoff: 3600 seconds (1 hour)

Run history is stored in JSONL format at `{savfox_home}/cron/history.jsonl`.

## CLI Management

```bash
savfox gateway cron list
savfox gateway cron create --name "daily" --schedule "0 9 * * *" --prompt "Good morning"
savfox gateway cron delete <job-id>
```
