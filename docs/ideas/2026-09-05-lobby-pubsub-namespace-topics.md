# Namespace-scoped pubsub topics for lobby rooms

Parked backlog (raised 2026-09-05). **Not started, and not a toolpath change** —
this is a design for `empathic/lobby`, recorded here because it was worked out
alongside the toolpath audit of the same session and the identity decisions bear
on how toolpath sessions join a knowledge graph. Kept on this standalone branch
(no PR) rather than a shipped tree.


Status: design, not implemented. Target: empathic/lobby.

## What exists today

Three pieces already do most of the work.

**Addressing is already a query.** `crates/lobby/src/tags.rs` defines `Query { pairs: Vec<(String,String)>, words: Vec<String> }`.
A `say` takes `to:` and parses it as one. Semantics in `Query::matches`: AND across
distinct keys, OR among a key's values, and — the load-bearing detail —
value comparison is `bv.to_lowercase().contains(w)`, a **substring match**, not equality.

**Rooms are derived from exactly one key.** `crates/lobby/src/rooms.rs::rooms_of()`:

    query.pairs.iter().filter(|(k, _)| k == "repo").map(|(_, v)| v.clone())

Every `repo:` term becomes a room. Every other key becomes routing-only —
it selects live sessions but is not logged anywhere.

**A room is a file path.** `rooms::path()` joins `state/rooms/{slug}.jsonl`,
and the comment is explicit that "the slug's own slash is the directory boundary" —
so `repo:empathic/toolpath` lands at `state/rooms/empathic/toolpath.jsonl`.
That is already a two-level hierarchy; it just isn't named as one.

The consequence worth stating plainly: **prefix subscription already half-works by
accident.** Because matching is `contains`, a `say to:"repo:empathic"` selects every
session tagged `repo:empathic/toolpath` and `repo:empathic/lobby`. But `rooms_of`
then logs it to `state/rooms/empathic.jsonl` — a *different* file from the two repo
rooms — so the live broadcast fans out and the durable log does not. Late joiners to
`empathic/toolpath` never see it. That inconsistency is the actual bug to fix, and
fixing it is most of the feature.

## The change

**A topic is an ordered tuple of `key:value` segments.** Not a new type — the
`Query.pairs` that already exist, kept in the order given rather than filtered
down to `repo`. `org:empathic repo:toolpath issue:141` is a topic of three segments.

**A room is any prefix of a topic.** Publishing to a 3-segment topic appends to all
three prefix rooms:

    org:empathic
    org:empathic/repo:toolpath
    org:empathic/repo:toolpath/issue:141

Subscribing to a prefix means reading that prefix's log. This is what makes
`org:repo:issue:pr` useful: a maintainer watching `org:empathic` hears everything;
someone on one issue hears only that issue. It is MQTT's topic hierarchy, but the
segments are typed key:value pairs rather than opaque path parts, which is what lets
the same string keep working as a session selector.

**Room ordering must be canonical, not as-typed.** `rooms_of` today sorts and dedups
values; a topic cannot, because order is meaning. So order by a declared key
precedence — `org < repo < issue < pr < *` — and sort unknown keys lexically after
the known ones. Otherwise `repo:toolpath org:empathic` and `org:empathic repo:toolpath`
are the same subscription writing to two different files, and every late joiner sees
half the history. This is the single easiest way to get this wrong.

**Path encoding.** Today the slug's slash *is* the separator, which works only
because `owner/repo` happens to be one slash. With arbitrary values the room path
must be built from encoded segments: percent-encode `/`, `:`, `.` and any
`..` in each value before joining, so `issue:../../etc` cannot escape
`state/rooms/`. Today `rooms::path` does `format!("{slug}.jsonl")` with no
validation at all — a `repo:` value with `../` in it already writes outside the
rooms directory. That is a real path-traversal bug in the current code, independent
of this feature, and it should be fixed first and separately.

**Backward compatibility.** `repo:owner/name` must keep resolving to
`state/rooms/owner/name.jsonl`. Treat the bare `repo:` key as sugar for
`org:owner repo:name` when its value contains exactly one slash, and keep the legacy
path as the room file for that prefix. Existing logs stay readable; nothing migrates.

## Identity, not location

Peer `pathbase-main-81` confirms pathbase already has `orgs`, `teams`, `repos`
entities in `crates/pathbase-db/src/`, with `activity_facts` keyed by
(day, repo, member, model) — and no channel/topic/pubsub anything. So there is no
namespace collision, but the first two segments *do* have real entities behind them.

Key those segments to pathbase **ids**, not names. Names change; a repo that is
renamed or transferred should not orphan its room log. `issue` and `pr` have no
pathbase entity today, so a topic will be part id-backed and part free string.

Make that explicit rather than implicit: each segment is either a **resolved entity
ref** or an **opaque label**, and the room record says which. Then pathbase can grow
issue/pr entities later and existing room logs stay valid — the segments just get
promoted from opaque to resolved. A room log that silently mixes the two is a
migration you cannot perform afterwards, because you can no longer tell which strings
were ever meant to be ids.

## What this does not change

The daemon stays the authority; `channel.rs` keeps its one `say` tool and its
`Identity::key()` addressing (`<session id>` or `guest:<owner/repo>`). Nothing
subscribes to the log — rooms.rs is explicit that "it is a file rather than a
protocol on purpose". Prefix rooms are still files, still read as a tail on connect.
The pubsub here is fan-out at publish time plus prefix reads, not a subscription
protocol. That is the right trade: it keeps a late joiner and a live listener seeing
the same thing, which is the property that currently breaks.

## Order of work

1. Fix the path traversal in `rooms::path` (separate, no feature).
2. Make the live fan-out and the durable log agree for the existing single-key case.
3. Generalize `rooms_of` to ordered topics + prefix rooms, with canonical key order.
4. Resolve org/repo segments against pathbase ids, recording resolved-vs-opaque.

Steps 1 and 2 are worth doing even if 3 and 4 never happen.
