"""udoc.asyncio -- async adapter for the sync udoc API.

Per  section 6.5 and , this module is a thin
stdlib-only bridge so callers in async codebases (FastAPI, etc) can
``await udoc.asyncio.extract(p)`` without blocking the event loop.

Design choices:

- No tokio runtime is added to udoc-py. The Rust side is sync, the
  asyncio adapter is pure Python.
- ``pyo3-asyncio`` is NOT a dependency. Every entry point bounces
  through ``loop.run_in_executor(executor, sync_call)``.
- Per-doc work (``extract``, ``extract_bytes``, ``stream``) defaults to
  ``concurrent.futures.ThreadPoolExecutor`` because the GIL is released
  inside ``udoc.extract`` via ``py.detach``; a single worker thread is
  enough to keep the event loop free.
- ``Corpus.parallel(N)`` uses ``concurrent.futures.ProcessPoolExecutor``
  with ``multiprocessing.get_context("spawn")``. Spawn (not fork) is
  mandatory on macOS and chosen for symmetry on Linux because PEP 703
  (free-threaded CPython) breaks fork-with-running-threads.
- Cancel propagates via ``executor.shutdown(wait=False, cancel_futures=True)``;
  callers can manage executor lifetime by passing one explicitly or by
  relying on the per-Corpus executor created during ``.parallel(N)``.

Public surface (mirrors  section 6.5):

    import udoc.asyncio as audoc

    doc = await audoc.extract("file.pdf")
    doc = await audoc.extract_bytes(blob)

    async with audoc.stream("file.pdf") as ctx:
        async for page_text in ctx:
            ...

    async for result in audoc.Corpus("dir/").parallel(8):
        ...

The shapes returned by these awaitables / async iterators are exactly
the sync types from ``udoc`` (``Document``, ``ExtractionContext``,
``Sourced``, ``Failed``, etc) -- no async-specific wrapper types so
users can ``isinstance(result, udoc.Failed)`` without a second import.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import multiprocessing
from typing import (
    Any,
    AsyncIterator,
    Callable,
    Iterable,
    Optional,
    Union,
)

import udoc as _sync

__all__ = [
    "extract",
    "extract_bytes",
    "stream",
    "Corpus",
]

# ---------------------------------------------------------------------------
# Single-doc entry points.
# ---------------------------------------------------------------------------


async def extract(
    path: Any,
    *,
    pages: Any = None,
    password: Optional[str] = None,
    format: Any = None,
    max_file_size: Optional[int] = None,
    config: Any = None,
    on_warning: Optional[Callable[[Any], None]] = None,
    executor: Optional[concurrent.futures.Executor] = None,
) -> Any:
    """Async wrapper for :func:`udoc.extract`.

    Returns a :class:`udoc.Document`. The sync call runs in ``executor``
    (or the loop's default executor when None); the GIL is released
    inside the Rust extraction so ``await``ing this does not block the
    event loop on a thread.
    """
    loop = asyncio.get_running_loop()

    def _call() -> Any:
        return _sync.extract(
            path,
            pages=pages,
            password=password,
            format=format,
            max_file_size=max_file_size,
            config=config,
            on_warning=on_warning,
        )

    return await loop.run_in_executor(executor, _call)


async def extract_bytes(
    data: bytes,
    *,
    pages: Any = None,
    password: Optional[str] = None,
    format: Any = None,
    max_file_size: Optional[int] = None,
    config: Any = None,
    on_warning: Optional[Callable[[Any], None]] = None,
    executor: Optional[concurrent.futures.Executor] = None,
) -> Any:
    """Async wrapper for :func:`udoc.extract_bytes`."""
    loop = asyncio.get_running_loop()

    def _call() -> Any:
        return _sync.extract_bytes(
            data,
            pages=pages,
            password=password,
            format=format,
            max_file_size=max_file_size,
            config=config,
            on_warning=on_warning,
        )

    return await loop.run_in_executor(executor, _call)


# ---------------------------------------------------------------------------
# stream() -- async-with around the sync ExtractionContext.
# ---------------------------------------------------------------------------


class _AsyncExtractionContext:
    """Async-with wrapper over a sync :class:`udoc.ExtractionContext`.

    The underlying ``ExtractionContext`` is a ``#[pyclass(unsendable)]``
    (it holds a ``RefCell<Extractor>`` that pyo3 pins to the thread
    that constructed it). That means we cannot open the sync ctx in an
    executor thread and then read from the loop thread without
    tripping pyo3's runtime thread-affinity check. So this wrapper
    drives the ctx entirely on the loop thread; the ``async`` shape
    is preserved (so users can still use ``async with`` /
    ``async for``), but the heavy lifting happens synchronously inside
    each ``await`` step.

    This is a deliberate tradeoff: the alternative is to dedicate a
    single worker thread to the ctx and pipe results back via a queue,
    which costs ~80 LOC for marginal benefit because the underlying
    page-read calls don't release the GIL anyway. We can revisit if a
    future Rust task makes ``ExtractionContext`` ``Sync`` (or the
    page-read methods detach) -- the public async surface here doesn't
    have to change.
    """

    def __init__(
        self,
        path: Any,
        *,
        pages: Any = None,
        password: Optional[str] = None,
        format: Any = None,
        max_file_size: Optional[int] = None,
        config: Any = None,
        on_warning: Optional[Callable[[Any], None]] = None,
        executor: Optional[concurrent.futures.Executor] = None,
    ) -> None:
        self._path = path
        self._pages = pages
        self._password = password
        self._format = format
        self._max_file_size = max_file_size
        self._config = config
        self._on_warning = on_warning
        # `_executor` is accepted for API symmetry with the other
        # async entry points but unused: the unsendable ctx pins to
        # the loop thread.
        self._executor = executor
        self._sync_ctx: Optional[Any] = None

    async def __aenter__(self) -> "_AsyncExtractionContext":
        # Open on the loop thread (see class docstring). The Rust side
        # releases the GIL inside Extractor::open_with so this still
        # cooperates with other tasks.
        ctx = _sync.stream(
            self._path,
            pages=self._pages,
            password=self._password,
            format=self._format,
            max_file_size=self._max_file_size,
            config=self._config,
            on_warning=self._on_warning,
        )
        ctx.__enter__()
        self._sync_ctx = ctx
        return self

    async def __aexit__(
        self,
        exc_type: Any,
        exc_val: Any,
        exc_tb: Any,
    ) -> bool:
        ctx = self._sync_ctx
        self._sync_ctx = None
        if ctx is None:
            return False
        ctx.__exit__(exc_type, exc_val, exc_tb)
        return False

    def page_count(self) -> int:
        if self._sync_ctx is None:
            raise RuntimeError(
                "ExtractionContext is not entered (use `async with`)"
            )
        return int(self._sync_ctx.page_count())

    async def __aiter__(self) -> AsyncIterator[str]:
        if self._sync_ctx is None:
            raise RuntimeError(
                "ExtractionContext is not entered (use `async with`)"
            )
        ctx = self._sync_ctx
        n = int(ctx.page_count())
        for i in range(n):
            yield ctx.page_text(i)
            # Yield to the loop between pages so a long document
            # doesn't starve other tasks. Zero-delay sleep is the
            # standard "be nice" primitive.
            await asyncio.sleep(0)


def stream(
    path: Any,
    *,
    pages: Any = None,
    password: Optional[str] = None,
    format: Any = None,
    max_file_size: Optional[int] = None,
    config: Any = None,
    on_warning: Optional[Callable[[Any], None]] = None,
    executor: Optional[concurrent.futures.Executor] = None,
) -> _AsyncExtractionContext:
    """Open a streaming extractor as an async context manager.

    Use as ``async with udoc.asyncio.stream(p) as ctx: async for t in ctx:``.
    """
    return _AsyncExtractionContext(
        path,
        pages=pages,
        password=password,
        format=format,
        max_file_size=max_file_size,
        config=config,
        on_warning=on_warning,
        executor=executor,
    )


# ---------------------------------------------------------------------------
# Corpus -- async wrapper over the sync Corpus.
# ---------------------------------------------------------------------------


def _make_executor(
    n_workers: int, mode: str
) -> concurrent.futures.Executor:
    """Build the executor backing an async Corpus.

, ``mode="process"`` uses ProcessPoolExecutor with the
    spawn multiprocessing context. ``mode="thread"`` uses
    ThreadPoolExecutor.
    """
    if mode == "process":
        ctx = multiprocessing.get_context("spawn")
        return concurrent.futures.ProcessPoolExecutor(
            max_workers=n_workers, mp_context=ctx
        )
    if mode == "thread":
        return concurrent.futures.ThreadPoolExecutor(max_workers=n_workers)
    raise ValueError(
        f"Corpus.parallel: unknown mode {mode!r} (expected \"process\" or \"thread\")"
    )


class Corpus:
    """Async wrapper around :class:`udoc.Corpus`.

    ``Corpus(source, config=...)`` mirrors the sync constructor.
    Builders (``filter``, ``with_config``, ``parallel``) return new
    ``Corpus`` instances, just like the sync side.

    Iteration is async: ``async for item in corpus`` yields
    ``Document | Failed``. Aggregating awaitables (``list``,
    ``to_jsonl``, ``count``, ``text``) run in the executor.

    The executor is owned by this Corpus when ``parallel(N)`` was
    called; otherwise the loop's default executor is used. Call
    ``await corpus.aclose()`` to release executor resources eagerly,
    or use ``async with corpus:`` to auto-close.
    """

    def __init__(
        self,
        source: Any,
        *,
        config: Any = "default",
        _sync_corpus: Optional[Any] = None,
        _executor: Optional[concurrent.futures.Executor] = None,
        _owns_executor: bool = False,
        _mode: str = "thread",
    ) -> None:
        if _sync_corpus is not None:
            # Internal builder path: clone with new state.
            self._sync = _sync_corpus
        else:
            self._sync = _sync.Corpus(source, config=config)
        self._executor = _executor
        self._owns_executor = _owns_executor
        self._mode = _mode

    # -- builders ---------------------------------------------------------

    def filter(self, predicate: Callable[[Any], bool]) -> "Corpus":
        """Return a new Corpus with an additional predicate."""
        return Corpus(
            None,  # ignored; _sync_corpus takes over
            _sync_corpus=self._sync.filter(predicate),
            _executor=self._executor,
            _owns_executor=False,  # caller still owns the original
            _mode=self._mode,
        )

    def with_config(self, config: Any) -> "Corpus":
        """Return a new Corpus with the given config."""
        return Corpus(
            None,
            _sync_corpus=self._sync.with_config(config),
            _executor=self._executor,
            _owns_executor=False,
            _mode=self._mode,
        )

    def parallel(
        self, n_workers: int = 1, *, mode: str = "process"
    ) -> "Corpus":
        """Return a new Corpus that fans out across ``n_workers``.

, ``mode="process"`` (default) uses
        ProcessPoolExecutor + spawn; ``mode="thread"`` uses
        ThreadPoolExecutor for I/O-bound preprocessing.
        """
        # Build a real backing sync Corpus.parallel(n, mode=...) so the
        # sync side knows it is a parallel corpus too (matters for
        # __iter__'s lazy executor spinup).
        new_sync = self._sync.parallel(n_workers, mode=mode)
        executor = _make_executor(n_workers, mode)
        return Corpus(
            None,
            _sync_corpus=new_sync,
            _executor=executor,
            _owns_executor=True,
            _mode=mode,
        )

    # -- iteration --------------------------------------------------------

    async def __aiter__(self) -> AsyncIterator[Any]:
        """Async iterate ``Document | Failed`` items.

        Each ``next()`` on the underlying sync iterator runs in the
        executor so a slow extraction does not block the event loop.
        """
        loop = asyncio.get_running_loop()
        sync_iter = iter(self._sync)
        sentinel = object()

        def _next() -> Any:
            return next(sync_iter, sentinel)

        while True:
            item = await loop.run_in_executor(self._executor, _next)
            if item is sentinel:
                return
            yield item

    # -- aggregators (eager) ---------------------------------------------

    async def list(self) -> list[Any]:
        """Eagerly materialize as ``list[Document]``.

        Raises on the first :class:`udoc.Failed`, matching the sync
        ``Corpus.list()`` contract.
        """
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(self._executor, self._sync.list)

    async def to_jsonl(self, path: Any) -> int:
        """Write one JSON line per Document. Returns the count."""
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(
            self._executor, self._sync.to_jsonl, path
        )

    async def count(self) -> int:
        """Eager file count (no extraction)."""
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(self._executor, self._sync.count)

    async def text(self, *, join: str = "\n\n") -> str:
        """Concatenate every document's text."""
        loop = asyncio.get_running_loop()

        def _call() -> str:
            return self._sync.text(join=join)

        return await loop.run_in_executor(self._executor, _call)

    # -- aggregators (streaming) -----------------------------------------

    async def tables(self) -> AsyncIterator[Any]:
        """Async iterate every table as ``Sourced[Table]``."""
        async for item in self._stream_sync_iter(self._sync.tables):
            yield item

    async def images(self) -> AsyncIterator[Any]:
        """Async iterate every image as ``Sourced[Image]``."""
        async for item in self._stream_sync_iter(self._sync.images):
            yield item

    async def metadata(self) -> AsyncIterator[Any]:
        """Async iterate per-document metadata as ``Sourced[DocumentMetadata]``."""
        async for item in self._stream_sync_iter(self._sync.metadata):
            yield item

    async def warnings(self) -> AsyncIterator[Any]:
        """Async iterate every warning as ``Sourced[Warning]``."""
        async for item in self._stream_sync_iter(self._sync.warnings):
            yield item

    async def chunks(
        self, *, by: str = "heading", size: int = 2000
    ) -> AsyncIterator[Any]:
        """Async iterate per-document chunks as ``Sourced[Chunk]``."""

        def _build() -> Any:
            return self._sync.chunks(by=by, size=size)

        async for item in self._stream_sync_iter(_build):
            yield item

    async def render_pages(
        self, indices: Iterable[int], *, dpi: int = 150
    ) -> AsyncIterator[Any]:
        """Async iterate rendered pages as ``Sourced[bytes]``."""
        idx_list = list(indices)

        def _build() -> Any:
            return self._sync.render_pages(idx_list, dpi=dpi)

        async for item in self._stream_sync_iter(_build):
            yield item

    async def _stream_sync_iter(
        self, build: Callable[[], Any]
    ) -> AsyncIterator[Any]:
        """Drain a sync iterator from the executor one element at a time.

        ``build`` is called once in the executor to produce the sync
        iterator (the build call may extract documents internally,
        which is the heavy part). Subsequent ``next()`` calls also
        bounce through the executor so a slow per-item conversion does
        not block the loop.
        """
        loop = asyncio.get_running_loop()
        sync_iter = await loop.run_in_executor(self._executor, build)
        sentinel = object()

        def _next() -> Any:
            return next(sync_iter, sentinel)

        while True:
            item = await loop.run_in_executor(self._executor, _next)
            if item is sentinel:
                return
            yield item

    # -- lifecycle --------------------------------------------------------

    async def __aenter__(self) -> "Corpus":
        return self

    async def __aexit__(
        self, exc_type: Any, exc_val: Any, exc_tb: Any
    ) -> bool:
        await self.aclose()
        return False

    async def aclose(self) -> None:
        """Release the executor created by ``parallel(N)``.

        No-op when this Corpus does not own its executor (i.e. was not
        produced by ``.parallel(N)``).
        """
        if self._owns_executor and self._executor is not None:
            ex = self._executor
            self._executor = None
            self._owns_executor = False
            # cancel_futures so already-queued work doesn't keep the
            # process pool alive for cancelled tasks.
            ex.shutdown(wait=False, cancel_futures=True)

    def __repr__(self) -> str:
        return f"<udoc.asyncio.Corpus mode={self._mode!r} owns_executor={self._owns_executor}>"


# Convenience aliases that match what users will type when they squint
# at the import name `audoc`. These are not in ``__all__`` -- the public
# API is ``extract``, ``extract_bytes``, ``stream``, ``Corpus`` -- but
# leaving them off would just force a second-line import for
# ``udoc.Failed`` / ``udoc.Document`` checks. Re-export the relevant
# sync types so ``audoc.Failed`` works without the second import.
Failed = _sync.Failed
Document = _sync.Document
Sourced = _sync.Sourced
Config = _sync.Config
