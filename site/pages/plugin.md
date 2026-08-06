---
layout: base.njk
title: Plugin
nav: plugin
permalink: /plugin/
---

# Claude Code Plugin

## Quick start

```
/plugin marketplace add empathic/toolpath
/plugin install path@toolpath
```

## `/path:query` — ask your session history anything

Your session history is sitting on your disk, but all the insights are
obfuscated behind an undocumented format that constantly churns and was
never meant to be read. `/path:query` ingests your sessions into the
stable [Toolpath format](/format/), and plain-English questions become
jq filters over the cache: computed, repeatable answers for a few
kilobytes of context, whatever harness they came from.

<div class="qa">
  <p class="qa-q">/path:query what did i work on last week?</p>
  <div class="qa-a">

Most of last week went to the **payments service**: migrating off the
legacy webhook handler (Tue–Thu, 640 steps) and two short sessions chasing
a race in the retry queue. Monday was the docs-site redesign; Thursday
afternoon a one-off backfill script. No recorded activity Friday or over
the weekend.

  </div>
</div>

<div class="qa">
  <p class="qa-q">/path:query which sessions cost me the most tokens this month?</p>
  <div class="qa-a">

The heaviest was Wednesday's session on the import-pipeline bug: roughly
**8.4M tokens** — 7.9M of it cache reads, 61k generated output. Worth
knowing: the top two sessions are segments of the same conversation;
together they're nearly half the month's total.

  </div>
</div>

<div class="qa">
  <p class="qa-q">/path:query compare my claude and codex usage this month</p>
  <div class="qa-a">

Claude Code carried most of the volume: 9.2M output tokens across 31
sessions, against Codex's 480k across 7. Codex runs shorter sessions — a
median of 41 steps to Claude's 220 — and spends about 4% of its output on
reasoning. Claude doesn't itemize thinking tokens, so its reasoning share
isn't measurable.

  </div>
</div>

<div class="qa">
  <p class="qa-q">/path:query what parts of my workflow were inefficient this month?</p>
  <div class="qa-a">

Two things dominate. About a fifth of the month's steps were later
rewound or replaced, concentrated in three sessions that each had to be
substantially redone. And marathon sessions are where the tokens go: the
five sessions over a thousand steps account for nearly half your context
spend — a fresh session per sub-task would be the highest-leverage change.
Tool failures barely register by comparison (2.3% of calls, mostly shell
friction).

  </div>
</div>

## `/path:share` — publish a session as a link

Publishes the current session to [Pathbase](https://pathbase.dev) as a
link anyone can open. Name another session in plain words ("the one about
the flaky test") and it finds and shares that one instead.

<div class="qa">
  <p class="qa-q">/path:share</p>
  <div class="qa-a">

Shared this session: `pathbase.dev/u/you/pathstash/webhook-migration`

  </div>
</div>

<div class="qa">
  <p class="qa-q">/path:share the session where we fixed the retry queue</p>
  <div class="qa-a">

Found it — Tuesday's session on the retry-queue race. Shared:
`pathbase.dev/u/you/pathstash/retry-queue-race`

  </div>
</div>

## `/path:link-pr` — attach the session to a pull request

Ship the _why_ with the diff. Shares the session and appends the link to
your pull request's description, so the review carries the conversation
behind the change:

<div class="qa">
  <p class="qa-q">/path:link-pr</p>
  <div class="qa-a">

Shared — the session is at
`pathbase.dev/u/you/pathstash/rate-limit-retry` and PR #212's description
now links to it.

  </div>
</div>

<div class="qa">
  <p class="qa-q">open a pr and link this chat</p>
  <div class="qa-a">

Opened PR #218 for this branch and linked the session in its
description: `pathbase.dev/u/you/pathstash/request-coalescing`

  </div>
</div>

## `/path:resume` — pick up a shared session anywhere

Sessions are portable. Point it at a Pathbase URL and it projects the
session into your project, ready to pick up with `/resume` — even if the
session started on another machine, or in a different harness entirely.

<div class="qa">
  <p class="qa-q">/path:resume pathbase.dev/u/mira/pathstash/retry-queue-race</p>
  <div class="qa-a">

Imported into this project. Run `/resume 7c9e2b41` to pick up where Mira
left off.

  </div>
</div>

## Installation

Inside Claude Code:

```
/plugin marketplace add empathic/toolpath
/plugin install path@toolpath
```

Or from your terminal:

```bash
claude plugin marketplace add empathic/toolpath
claude plugin install path@toolpath
```

The commands run on the `path` binary. On first use the plugin picks up a
`path` already on your `PATH`, or installs one from a GitHub release
(sha256-verified) to `~/.local/bin`. Everything happens locally — sessions
are ingested and queried on your machine, and only `/path:share` and
`/path:link-pr` upload anything, anonymously or
[signed in](https://pathbase.dev). The plugin is
[open source](https://github.com/empathic/toolpath/tree/main/plugins/claude-code),
and the same workflows are available from the [CLI](/cli/) directly.
