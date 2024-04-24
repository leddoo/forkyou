
- todo:
    - latch.
        - atomic state + worker index.
        - work until (latch).
    - wake more workers.
        - track sleeping workers.
        - per worker counter.
        - pop & steal return number of items left in queue.
    - move stuff into sti.
    - sti mem imports.
    - untracked tasks block terminator.
        - check if terminator present, then fetchadd untracked counter.
    - panic handling.
        - and abort on unwind inside lib.

