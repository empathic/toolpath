---
layout: base.njk
title: "Path Kinds"
permalink: /kinds/
---

# Path Kinds

A Toolpath path's optional `meta.kind` is a URI naming a _kind specification_: a contract describing the additional shape the path follows on top of the base format. Consumers that recognize the URI may rely on the structure that spec describes. Unrecognized URIs should be treated as a generic path.

Kind URIs are immutable: revisions ship at a new version URI, and old URIs keep meaning what they always meant. Versioning follows [semver](https://semver.org/).

## Defined kinds

| Kind                                                   | Current URI                                              | Spec                                          |
| ------------------------------------------------------ | -------------------------------------------------------- | --------------------------------------------- |
| [`agent-coding-session`](/kinds/agent-coding-session/) | `https://toolpath.dev/kinds/agent-coding-session/v1.0.0` | [v1.0.0](/kinds/agent-coding-session/v1.0.0/) |
