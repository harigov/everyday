# Reminders, and tasks the assistant gives itself

> **Delivered** on `bad-octopus` (Oct 27 2026). Hari's calls:
>
> - **The reminder time lives on the task**, not in a reminders list of its
>   own. "Remind me at five to call the dentist" is a task with a reminder.
> - **Your task reminders arrive on the desktop; the assistant's arrive in
>   your chat.** Both can be changed in Settings → Assistant → Reminders.

## What a reminder is

`Task.remind_at` is a moment, not a day, and is separate from the deadline.
`Task.reminded_at` records when it last went out. A reminder is waiting while
`reminded_at` is unset or earlier than `remind_at`. So "again in an hour" is
just a later `remind_at`, and nothing has to clear the stamp.
`Task::next_reminder` holds that rule, and `nextReminder` in
`ui/src/lib/tasklist.ts` mirrors it, with a test pinning the two together.

No schema migration was needed. `TaskQuery` gained `assignee` and
`reminders_due_by`, and both are answered by the in-memory pass after
decryption, the way tags and text already are. The scheduler asks for open
tasks only, and status is a clear column.

## Whose task it is

`Task.assignee` is `owner` (the default, and every older task) or `assistant`.

- **Your task.** When its reminder comes round, `reminders::sweep` (on the
  scheduler's minute) sends the title, plus the due date if there is one, to
  wherever `ReminderSettings::yours` points. No model is involved, so a
  reminder cannot fail the way a routine can.
  - In the chat, each reminder gets a thread of its own holding what was
    sent, so a reply of "done" or "again at six" has context to work from.
  - A chat that is not connected sends the reminder to the desktop instead.
  - A reminder missed by more than a day (the app was shut) is stamped and
    not sent.
- **The assistant's task.** When its time comes, a `JobKind::Task` is
  queued. The task moves to Doing when the job starts, not when it is
  queued, so a job cancelled before then leaves nothing claiming to be
  under way. It can move to Doing from any open state, Blocked included.
  - With no time set, it starts at once. Handing a task over means now.
  - It starts however late, and the prompt says when it was meant for.
    Only your own reminders are dropped as missed after a day.
  - Handing over a task whose reminder already went out clears the stamp,
    so the assistant's start is due afresh.
  - `jobs::task` runs it as an unattended job with the job rules: no deletes,
    no sends.
  - The final reply goes to wherever `ReminderSettings::assistants` points.
    In the chat it threads onto the job's transcript.
  - Delivery is owed until done. `Job::answered` marks it sent, and the
    worker offers an unsent result on every pass, the way it does research
    answers.
  - The job may open the addresses its task names, as long as they carry
    nothing data-like.
  - A finished job leaves the task Done unless the model moved it itself, for
    example to Blocked. A failed or cancelled job puts it back to To do.
  - A task closed while its job waited is refused rather than done anyway,
    and nothing is sent about it.

## Who may hand the assistant work

The rail, the chat and MCP can. A routine, a dream (even as a proposal), a
background job, or the turn that comes back with a job's answer cannot
(`refuse_handing_over` in `agent/tools/tasks.rs`). That means it cannot:

- create an assistant task;
- hand one of your tasks to the assistant;
- change what one of the assistant's tasks will do: its title, its notes,
  when it starts;
- reopen one once it is closed.

Closing one, as Done or Blocked, is allowed, since that is how its own job
finishes it.

It is the same recursion `create_routine` refuses. An assistant task is a
future model run with every tool, on instructions the model wrote itself, and
a run nobody is watching may have written them from a stranger's mail.

Unattended runs **may** still set reminders on your tasks, since a reminder
is only a message to you. The morning brief's "want me to sort the transfer?"
works because your "yes" in Matrix is a chat turn, not an unattended one.

## Also changed

- Reminders and assignees ride in the hidden comment of an exported task
  line, as `for:`, `remind:` and `reminded:`.
- `JobTurn.context` lets each kind of job supply its own turn context. It
  used to be research's for every job.
- `escape_markdown` moved from the chat relay into `everyday_service::chat`,
  so reminders can use it too.
- The calendar guidance no longer sends "a reminder at a time" to the
  calendar, because an event never pings anyone. It now points to
  `remind_time` on a task.

## Left open

- Notification clicks still aren't routed, so a desktop reminder doesn't open
  its task.
- The assistant cannot schedule a retry for itself. A failed task stays open
  and says why, and a later time in the editor runs it again.
- There's no "Assistant" view in the Tasks sidebar yet. Those tasks carry a
  chip, and `list_tasks` can filter them by `assignee`.
