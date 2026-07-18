# Chronicle architecture

The capture path is intentionally independent from the AI path:

```text
Windows provider -> normalized raw event -> SQLite -> Processing Queue -> semantic event/search index
```

Raw events are append-only evidence. Semantic events reference their source raw event and can be regenerated when models change.

