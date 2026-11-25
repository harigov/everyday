# Prompt review: area dossiers

The detailed reviews behind [`../prompt-review.md`](../prompt-review.md),
one per area, written on 24 November 2026 against `f4e9acea`. Each covers
every prompt in its area: where it lives and which model runs it, what the
feature is for, what the model is actually sent, the current text verbatim, a
diagnosis, a complete replacement, and the tests a rewrite would break. Each
ends with a ranked list of changes.

| Dossier | Covers | Main doc |
|---|---|---|
| [`mail-intelligence.md`](mail-intelligence.md) | Small-model categorising and Priority, Summarise and Listen, auto-draft's decision, follow-up questions, decision-model sorting and the local d3 template, send checks, reading an email aloud, `summarize_thread` | §9 |
| [`meetings-people.md`](meetings-people.md) | Meeting notes, naming voices, titles, step ownership, meeting briefs, the `meeting_brief` tool, keeping in touch's journal read | §10 |
| [`briefs-dreams-jobs-quick.md`](briefs-dreams-jobs-quick.md) | Morning, evening and weekly briefs, the daily podcast, the three dreams, research, project, task and watch jobs, job follow-ups and tool descriptions, all 23 quick asks, routine templates | §11 |
| [`feed-health-speech-misc.md`](feed-health-speech-misc.md) | Feed ranking, summaries, podcast articles and Listen; lab extraction, meal photos and the visit brief; speech-recognition hints; picture tools; the MCP server's instructions; `Profile::describe` | §12 |

The assistant's own prompt, the tool catalogue, voice mode, mail writing and
writing at the desk are reviewed in the main document itself (§2-§8).

Nothing here has been run against a real model. Line numbers are as of
`f4e9acea` and will drift.
