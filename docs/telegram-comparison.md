# rchat benchmark — Telegram-level comparison (auto-target)
# Automation: read this file every run; if item missing → ADD it.
MANDATORY TARGETS:
- Mobile native build docs / SDK (iOS/Android)
- Production Dockerfile + docker-compose
- Release artifacts (.AppImage / .deb / macOS / binary)
- CI release pipeline (not only fmt/clippy/check)
- Bot / webhook API stub (Telegram-style integration)
- Analytics / metrics (usage, errors, retention)
- EN documentation (README install, protocol)
- Full integration/e2e tests (not smoke-only)
- Multi-device sync / server-side history search

RULE: If branch is missing any above → add real scaffold, never skip.
