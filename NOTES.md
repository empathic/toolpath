* How do we consider the attribution for tools that use source maps
  (https://tc39.es/ecma426/2024/)?
* toolpath.net, not toolpath.dev
```
  step        =A single change to artifact(s)
  path        =Linear sequence of steps
  path tree   =Full DAG including abandoned branches
  current path=Steps leading to current state
  dead end    =Abandoned path
  fork        =Where paths diverge
  backtrack   =Abandoning steps to try another path 
```
* Step should optionally have some form of ref for root context.  Maybe a URL to
  GitHub, Bitbucket, filesystem, etc. and commit hash, change ref, etc.?
* What would it look like to render a toolpath decision tree like deciduous.app?
* We should include some "on behalf of" concept.
