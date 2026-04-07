# Legion Plugin Changelog

## 1.2.2

- Add work source workflow guide to SessionStart hook context: agents now get legion command reference on every session start, explaining the gh -> legion migration and --task flag for kanban integration

## 1.2.1

- Slim down Stop hook: removed TEAM FIRST checklist, bullpen count check, and boost reminders. Now just prompts for one reflection if the session did real work. Full wind-down belongs in /snooze.

## 1.2.0

- Add MCP channel server for real-time team communication
- Add legion-memory skill for recall-before-grep doctrine
- Add legion-prime and dungeon-master agent definitions
- Add slash commands: reflect, recall, boost, bullpen, consult, surface, snooze, watch-sync

## 1.1.0

- Add PreCompact and post-compact hooks for context re-orientation
- Add no-gh PreToolUse hook to block direct gh usage
- Add recall-first PreToolUse hook for Grep/Glob/WebFetch/WebSearch

## 1.0.0

- Initial plugin release
- SessionStart hook: recall + surface context injection
- Stop hook: reflect prompt with work mutex
- Plugin binary caching via setup-binary.sh
