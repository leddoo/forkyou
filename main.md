
- todo:
    - map rework.
        - `[MaybeUninit<U>]`.
    - wake more workers.
    - work stealing.
    - wake fewer workers.
    - untracked tasks block terminator.
        - check if terminator present, then fetchadd untracked counter.
    - panic handling.
        - and abort on unwind inside lib.

