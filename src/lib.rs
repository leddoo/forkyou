#![forbid(unsafe_op_in_unsafe_fn)]

use sti::alloc::Alloc;
use sti::boks::Box;
use core::ptr::NonNull;
use core::mem::ManuallyDrop;
use core::marker::PhantomData;

pub mod deque;
pub mod state_cache;
pub mod spliterator;

pub use state_cache::StateCache;
pub use spliterator::{Spliterator, IntoSpliterator, IntoSpliteratorRef, IntoSpliteratorRefMut};

mod runtime;
mod untracked;
mod scope;
mod join;
mod map;
mod for_each;

pub use runtime::Terminator;
pub use untracked::spawn_untracked;
pub use scope::scope;
pub use join::{join, join_on_worker};
pub use map::{map, map_in, map_into, map_core};
pub use for_each::for_each;


// temp sti stuff:
type CovariantLifetime<'a>     = PhantomData<fn()       -> &'a ()>;
type ContravariantLifetime<'a> = PhantomData<fn(&'a ())>;
type InvariantLifetime<'a>     = PhantomData<fn(&'a ()) -> &'a ()>;
fn box_into_inner<T, A: Alloc>(this: Box<T, A>) -> T {
    let (ptr, alloc) = this.into_raw_parts_in();
    let ptr = ptr.cast::<ManuallyDrop<T>>();
    let mut this = unsafe { Box::from_raw_parts_in(ptr, alloc) };
    let result = unsafe { ManuallyDrop::take(&mut *this) };
    return result;
}


#[derive(Clone, Copy, Debug)]
struct XorShift32 {
    pub state: u32,
}

impl XorShift32 {
    #[inline]
    pub fn new() -> Self {
        // pi in fixed point.
        Self { state: 0x517cc1b7 }
    }

    #[inline]
    pub fn from_seed(seed: u32) -> Self {
        Self { state: seed }
    }

    #[inline]
    pub fn next(&mut self) -> u32 {
        /* Algorithm "xor" from p. 4 of Marsaglia, "Xorshift RNGs" */
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        return x;
    }

    #[inline]
    pub fn next_n(&mut self, n: u32) -> u32 {
        ((self.next() as u64 * n as u64) >> 32) as u32
    }
}



use runtime::Runtime;



#[inline]
pub fn ncpu() -> usize {
    thread_local! {
        static NCPU: usize =
            if cfg!(not(miri)) {
                std::thread::available_parallelism()
                    .map_or(1, |n| n.get())
            }
            else { 4 };
    }

    NCPU.with(|ncpu| *ncpu)
}



pub struct Task {
    ptr: NonNull<u8>,
    call: fn(NonNull<u8>),
}

impl Task {
    #[inline]
    pub unsafe fn new(ptr: NonNull<u8>, call: fn(NonNull<u8>)) -> Self {
        Self { ptr, call }
    }

    #[inline]
    pub fn call(self) {
        (self.call)(self.ptr);
    }
}

unsafe impl Send for Task {}





/// tasks can borrow locals from parent.
/// ```rust
/// let _t = forkyou::Terminator::new();
/// let foo = 42;
/// forkyou::scope(|scope| {
///     scope.spawn(|| foo);
/// });
/// ```
///
/// tasks cannot borrow locals from scope.
/// this is necessary because tasks may not complete before the closure returns.
/// ```compile_fail
/// forkyou::scope(|scope| {
///     let foo = 42;
///     scope.spawn(|| foo);
/// });
/// ```
///
/// scope can assign locals from parent.
/// ```rust
/// let _t = forkyou::Terminator::new();
/// let mut foo = None;
/// forkyou::scope(|scope| {
///     foo = Some(42);
/// });
/// ```
///
/// single task can assign locals from parent.
/// ```rust
/// let _t = forkyou::Terminator::new();
/// let mut foo = None;
/// forkyou::scope(|scope| {
///     scope.spawn(|| foo = Some(42));
/// });
/// ```
///
/// multiple tasks cannot mutably borrow locals from parent.
/// ```compile_fail
/// let mut foo = None;
/// forkyou::scope(|scope| {
///     scope.spawn(|| foo = Some(42));
///     scope.spawn(|| foo = Some(42));
/// });
/// ```
///
/// allocations cannot escape scope.
/// this is necessary for the use Arena::save/restore to be safe.
/// ```compile_fail
/// let mut foo = &mut 42;
/// forkyou::scope(|scope| {
///     foo = scope.alloc().alloc_new(42);
/// });
/// ```
///
/// tasks cannot access scope.
/// this is necessary for the use Arena::save/restore to be safe.
/// ```compile_fail
/// forkyou::scope(|scope| {
///     scope.spawn(|| {
///         scope.alloc().alloc_new(42);
///     });
/// });
/// ```
///
/// inner scope cannot access outer scope.
/// this is necessary for the use Arena::save/restore to be safe.
/// ```compile_fail
/// forkyou::scope(|scope| {
///     forkyou::scope(|_| {
///         scope.alloc().alloc_new(42);
///     });
/// });
/// ```
#[allow(dead_code)]
struct DocTests;

