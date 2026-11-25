---
description: Store a reflection for future recall
argument-hint: "<reflection text> [--domain <domain>] [--tags <tags>]"
allowed-tools: ["Bash"]
---

Run `legion reflect --text '$ARGUMENTS'`.

If the user included --domain or --tags flags, pass them through. If not, just store the text.

Confirm the reflection was stored.
