
- "the runtime stuff".
    - fix scope.
    - untracked tasks block terminator.
        - check if terminator present, then fetchadd untracked counter.
    - wake more workers.
    - work stealing.

- todo:
    - move stuff from lib into sub modules.

- spliterator & map rework.
    - len, split.
    - `[MaybeUninit<U>]`.
    - test lying spliterator.

- panic handling.
    - and abort on unwind inside lib.

