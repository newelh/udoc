"""Type stubs for `udoc.asyncio`.

The async adapter is a thin wrapper over the sync API for codebases that
are async-shaped (FastAPI, etc.). Implementation runs the sync calls in
a default executor (`loop.run_in_executor`); for `Corpus` it bridges to
a `concurrent.futures.ProcessPoolExecutor` so parallel extractions are
not GIL-bound (DEC-143).

Per phase-16-plan Sec 6.5, only the headline shapes are async:

  * `extract`, `extract_bytes` -- one-shot.
  * `stream` -- async-with context manager.
  * `Corpus` -- async iter + async aggregators.

CPU-trivial helpers like `detect_format` are not duplicated here -- callers
use the sync versions directly. `udoc.asyncio.Corpus` is a different class
than `udoc.Corpus`; results yielded from async iteration are still the
plain sync `Document` / `Failed` / `Sourced[T]` types from the parent module.
"""

from __future__ import annotations

from typing import (
    Any,
    AsyncIterator,
    Callable,
    Optional,
    Sequence,
    Union,
)

from udoc import (
    Chunk,
    ChunkBy,
    Config,
    Corpus as _SyncCorpus,
    CorpusMode,
    CorpusSource,
    Document,
    DocumentMetadata,
    ExtractionContext,
    Failed,
    Format,
    Image,
    PagesArg,
    PathLike,
    Sourced,
    Table,
    Warning,
)

# ---------------------------------------------------------------------------
# Top-level coroutines.
# ---------------------------------------------------------------------------

async def extract(
    path: PathLike,
    *,
    pages: PagesArg = None,
    password: Optional[str] = None,
    format: Union[Format, str, None] = None,
    max_file_size: Optional[int] = None,
    config: Optional[Config] = None,
    on_warning: Optional[Callable[[Warning], None]] = None,
) -> Document:
    """Async wrapper around `udoc.extract`. Runs in the default executor."""
    ...

async def extract_bytes(
    data: bytes,
    *,
    pages: PagesArg = None,
    password: Optional[str] = None,
    format: Union[Format, str, None] = None,
    max_file_size: Optional[int] = None,
    config: Optional[Config] = None,
    on_warning: Optional[Callable[[Warning], None]] = None,
) -> Document:
    """Async wrapper around `udoc.extract_bytes`."""
    ...

# ---------------------------------------------------------------------------
# Async streaming context manager.
# ---------------------------------------------------------------------------

class AsyncExtractionContext:
    """Async-with wrapper over a sync `ExtractionContext`.

    The underlying extractor runs in a thread; page accessors are
    coroutines that await `loop.run_in_executor(None, ...)`.
    """

    async def __aenter__(self) -> AsyncExtractionContext: ...
    async def __aexit__(
        self,
        exc_type: Optional[type[BaseException]] = None,
        exc_val: Optional[BaseException] = None,
        exc_tb: Optional[Any] = None,
    ) -> bool: ...
    async def close(self) -> None: ...
    def page_count(self) -> int: ...
    def __len__(self) -> int: ...
    async def page_text(self, index: int) -> str: ...
    async def page_lines(
        self, index: int
    ) -> list[tuple[str, float, bool]]: ...
    async def page_spans(
        self, index: int
    ) -> list[tuple[str, float, float, float, float]]: ...
    async def page_tables(self, index: int) -> list[list[list[str]]]: ...
    async def page_images(self, index: int) -> list[dict[str, Any]]: ...

def stream(
    path: PathLike,
    *,
    pages: PagesArg = None,
    password: Optional[str] = None,
    format: Union[Format, str, None] = None,
    max_file_size: Optional[int] = None,
    config: Optional[Config] = None,
    on_warning: Optional[Callable[[Warning], None]] = None,
) -> AsyncExtractionContext:
    """Open a streaming extractor as an async context manager.

    Returned object is awaitable via `async with udoc.asyncio.stream(...) as ext:`.
    """
    ...

# ---------------------------------------------------------------------------
# Async Corpus.
# ---------------------------------------------------------------------------

class Corpus:
    """Async-shaped wrapper around `udoc.Corpus`.

    The sync `Corpus` is held internally; iteration is async (each step
    awaits a future from a `ProcessPoolExecutor`). Aggregator methods
    return `AsyncIterator[Sourced[T]]`.
    """

    def __init__(
        self,
        source: CorpusSource,
        *,
        config: Union[Config, str, None] = None,
    ) -> None: ...
    def __aiter__(self) -> AsyncIterator[Union[Document, Failed]]: ...
    def filter(self, predicate: Callable[[Document], bool]) -> Corpus: ...
    def with_config(self, config: Union[Config, str]) -> Corpus: ...
    def parallel(
        self,
        n_workers: int = 1,
        *,
        mode: CorpusMode = "process",
    ) -> Corpus: ...
    def tables(self) -> AsyncIterator[Sourced[Table]]: ...
    def images(self) -> AsyncIterator[Sourced[Image]]: ...
    def chunks(
        self,
        *,
        by: ChunkBy = "heading",
        size: int = 2000,
    ) -> AsyncIterator[Sourced[Chunk]]: ...
    def metadata(self) -> AsyncIterator[Sourced[DocumentMetadata]]: ...
    def warnings(self) -> AsyncIterator[Sourced[Warning]]: ...
    def render_pages(
        self,
        indices: Sequence[int],
        *,
        dpi: int = 150,
    ) -> AsyncIterator[Sourced[bytes]]: ...
    async def list(self) -> list[Document]: ...
    async def to_jsonl(self, path: PathLike) -> int: ...
    async def text(self, *, join: str = "\n\n") -> str: ...
    def count(self) -> int: ...
