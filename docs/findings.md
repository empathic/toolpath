# Where a finding goes

A "finding" is anything an audit, a review, or a stray afternoon turns up that
looks wrong: a comment the code has outgrown, a claim the RFC makes that nothing
implements, a measurement that does not match what a field's name promises.

This document exists because a finding with no defined destination becomes an
issue on a public repo by default, and default is the wrong answer often enough
to be worth a page.

## First, check whether it is already known

In this order. Stop at the first hit.

1. **Open and closed issues.** Closed ones matter — a finding may have been
   considered and rejected, and that reasoning is what you actually want.
2. **Open PRs.** Long-lived branches absorb findings silently; a doc fix can be
   sitting in a PR that grew into a feature and stalled.
3. **`docs/ideas/`.** Parked backlog notes. These are the easiest to miss and the
   most expensive to duplicate, because they often contain a *correction* to the
   surface reading you just arrived at independently.
4. **`notes/`** and **`docs/agents/formats/*/known-gaps*`**.
5. **`RFC.md` and `FAQ.md` open-questions sections.**

**A finding that duplicates a parked note is an amendment to that note, not a new
issue.** If the note reached a different conclusion than you did, say why yours
differs rather than restating the observation — and consider that the note may be
right.

## Then route it by kind

| The finding… | goes to |
|---|---|
| contradicts the code | a PR that fixes it |
| contradicts the RFC or the schema | an issue, `spec` in the title |
| needs a design decision before anyone can fix it | an issue, with the measurement attached |
| is speculative, unbounded, or blocked on something else | `docs/ideas/YYYY-MM-DD-<slug>.md`, no PR |

Prefer a PR when you are going to fix it yourself. File an issue for what you are
*not* fixing — an issue is a request for someone else's judgement, and if you have
already made the judgement it should be a PR.

## Evidence bar

Every finding carries `file:line`. Every claim about *behaviour* carries a command
that reproduces it.

This is not ceremony. An adversarial refutation pass over 40 claims from one audit
in September 2026 refuted 6 outright and judged 7 more true-but-trivial — a 32%
error rate on claims that had already been written down as findings. Plausible
readings of unfamiliar code are wrong about a third of the time, and they are
wrong in a way that reads fluently. The reproducing command is what separates the
two.

Three failure modes worth naming, all seen in that audit:

- **Inferring a consequence the code does not support.** The observation is
  correct, the "and therefore" is not. Trace the consequence to source too.
- **Verifying against a built artifact.** A compiled or vendored bundle is
  authoritative for what *ran*, and silently lags what is being written. Scope the
  claim to what you actually checked, not to the project as a whole.
- **A zero that came from a selector, not from the data.** A filter on a field
  that does not exist yields an empty result rather than an error, so it reports
  a confident zero. Any finding of the form "the count of X is zero" must name
  the field it selected on and show that the field exists. The concrete instance:
  a wrapped step has `.step.actor`, and no `.role` — so
  `select(.role == "user")` returns `[]` and "zero user steps" looks measured.
  This is the most dangerous of the three, because a zero is the one result
  nobody thinks to sanity-check: an unexpected number invites a second look and
  an expected absence does not.

When two careful readings disagree, no amount of re-reading resolves it. Find a
value the system has already produced that both readings predict differently, and
let it arbitrate.

**A finding you relayed is not a finding you verified.** A number that arrives
from a subagent, a colleague, or your own earlier notes carries no evidence about
the method that produced it. Before it goes in an issue, re-derive it — or say
plainly in the issue that you did not.

## Public-repo rule

This repository is public. Nothing that names a personal branch, a machine path,
an internal host, or a private repository belongs in it — including in issues, PR
bodies, and commit messages, which are as public as the code.

If a finding can only be explained by referring to one of those, the explanation
belongs in `docs/ideas/` on a branch that does not ship, and the public artifact
gets the part that stands on its own.
