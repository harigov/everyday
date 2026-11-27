# Karma

Karma is the application's points: what each day's deeds came to, for better
and for worse. Ranks and Siddhis will be built on it later; they are not
built yet.

## What was decided

- **Each app decides what earns Karma, and it must not be easy to earn.**
  Every source has its own daily cap, and a day has a ceiling of 100
  ("a well-lived day").
- **Karma can be lost.** A habit being broken, like a drink or a cigarette,
  costs Karma on each day it happens. Losses are never capped.
- **Karma starts at zero.** Nothing from before the day it started counts.

## Rules (version 1)

| Source | Earns | Daily cap |
|---|---|---|
| A task finished (yours, not the assistant's) | 5, or 10 if filed under a goal or role (directly or through its project) | 40 |
| A tracker on a day it counts | the tracker's `karma`, or the default below | 40 for gains, none for losses |
| Writing in the journal, on the day or the morning after | 10, once | 10 |
| Recorded time filed under a goal or role | 5 per whole hour | 20 |
| Something finished on a shelf | 10 each | 20 |

A tracker's `karma` is signed and held to ±50:

- A positive value earns on a day the habit is **kept**. Something has to be
  recorded (a tick of "done", not a deliberate "not done"), and the day must
  not fall short of its daily target, if it has one.
- A negative value costs on a day the habit **slipped**. Something has to be
  recorded, and the day has to go over its daily target, if it has one. So
  "Alcohol" with no target costs on every day a drink is recorded, and "at
  most an hour of TV" costs only on a day past the hour. A weekly or yearly
  limit can't be judged one day at a time, so it costs on every day the
  habit happens.
- A recorded zero, of any kind ("no drinks today", "0 minutes"), is a day
  the habit did not happen. It neither earns nor costs.

When `karma` is unset, the default applies:

- A check or amount tracker earns 5, or costs 5 if it is shaped like a limit:
  a maximum on some target and a minimum on none, whatever the period.
- A dose or symptom tracker earns nothing.
- Screen-time and Health trackers cost 5 when shaped like a limit, earn 5
  when they have a minimum, and earn nothing otherwise.
- Time and finished trackers earn nothing, because those deeds are already
  counted as their own sources.

## Shape

- `jatayu_core::karma` holds the rules as pure functions, tested there.
  `ui/src/lib/karma.ts` mirrors them for the mock backend and the tracker
  editor, and `ui/scripts/karma.test.mjs` pins the same cases.
- `Vault::karma(from, to, today)` is a read. It scores the open days (the
  last week or so) and the window being drawn, from the records, and adds
  the settled days' sum. Nothing is stored per deed, so a reopened task or a
  corrected reading changes the open days it touches.
- The only stored state is `KarmaBook`, a sealed singleton (schema v33,
  `karma_book`). It holds `since` and the **settled** days: each day's
  earned, lost and net, once the day is more than seven days old. A settled
  day is never re-scored. That bounds a report's work, and it means a later
  change to the rules, or to what a tracker is worth, cannot rewrite the
  past.
- `Vault::settle_karma(today)` is the only writer. It starts the book on
  first use and settles what is due. The service runs it in the background
  after an unlock, and only in a process that may write.
- The `karma` command (scope `All`) returns a `KarmaReport`: since, total,
  today, the days in the window, and the rules version.
- The Overview has a Karma card: the total, today's net, and a year's
  heatmap. The heatmap is diverging: teal for earned, orange for lost, and
  neutral for zero. Hovering a day lists its deeds.
- The tracker editor (Journal settings) has a Karma choice for each tracker.

## Not built yet

- Ranks, levels and Siddhis.
- An assistant tool to read Karma.
- Gating back-dated readings. A reading logged a week late for an earlier
  day still counts that day.
