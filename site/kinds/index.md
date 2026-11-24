---
layout: base.njk
title: "Path Kinds"
permalink: /kinds/
---

# Path Kinds

A Toolpath path's optional `meta.kind` is a URI naming a _kind specification_: a contract describing the additional shape the path follows on top of the base format. Consumers that recognize the URI may rely on the structure that spec describes. Unrecognized URIs should be treated as a generic path.

Kind URIs are immutable: revisions ship at a new version URI, and old URIs keep meaning what they always meant. Versioning follows [semver](https://semver.org/).

## Defined kinds

| Kind                                                   | URI                                                      | Spec                                          | Status                                                                                           |
| ------------------------------------------------------ | -------------------------------------------------------- | --------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| [`agent-coding-session`](/kinds/agent-coding-session/) | `https://toolpath.net/kinds/agent-coding-session/v1.1.0` | [v1.1.0](/kinds/agent-coding-session/v1.1.0/) | **current** — the only version producers write                                                   |
| [`agent-coding-session`](/kinds/agent-coding-session/) | `https://toolpath.net/kinds/agent-coding-session/v1.0.0` | [v1.0.0](/kinds/agent-coding-session/v1.0.0/) | **read-only legacy** — superseded 2026-06-17; not supported for writing; `path p validate` warns |

A superseded URI stays valid and keeps its meaning (that is the immutability rule above), so readers keep accepting it. "Not supported for writing" means no toolpath producer emits it and new documents must not carry it; the v1.0.0 page records the erratum that made the revision necessary.
