
- todo:
    - move stuff into sti.
    - sti mem imports.
        - and clean up crate imports. don't reexport private stuff, who cares.
    - panic handling.
        - and abort on unwind inside lib.
    - bsp tracing.
    - consider buffer pool.
    - consider ebr.
    - consider lock free `StateCache`, cause rn it's kinda useless.
        - would box list nodes and give out pointer to list node in
          the `Cached` thing, which returns to `StateCache` on drop.
          and `StateCache` frees the nodes.
          then we shouldn't have any issues with `free`.
    - untracked tasks block terminator.
        - check if terminator present, then fetchadd untracked counter.
    - scope struct.
        - arc and ig last task runs cleanup.

