
- todo:
    - wake more workers.
        - track sleeping workers.
        - wake on submit if threads are sleeping.
    - adaptive splitting.
    - move stuff into sti.
    - sti mem imports.
    - panic handling.
        - and abort on unwind inside lib.
    - untracked tasks block terminator.
        - check if terminator present, then fetchadd untracked counter.

