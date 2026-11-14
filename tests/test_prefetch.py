"""Behavioral prefetch regressions; no running Hermes or remote service needed."""
import contextvars
import importlib.util
import json
from pathlib import Path
import sys
import threading
import time
import types
from unittest.mock import Mock, patch

import pytest


def spawn(fn, name=""):
    context = contextvars.copy_context()
    return threading.Thread(target=lambda: context.run(fn), name=name, daemon=True)


def load_plugin():
    host = types.ModuleType("agent.memory_provider")
    host.MemoryProvider = object
    host.spawn_context_thread = spawn
    registry = types.ModuleType("tools.registry")
    registry.tool_error = lambda msg: json.dumps({"error": msg})
    with patch.dict(sys.modules, {"agent.memory_provider": host, "tools.registry": registry}):
        spec = importlib.util.spec_from_file_location("prefetch_plugin", Path(__file__).parents[1] / "plugins" / "memex8" / "__init__.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
    return module


plugin = load_plugin()


def provider(client=None, **config):
    p = plugin.Memex8MemoryProvider()
    p._session_id = "session-a"
    p._config = {"recall_top_k": 3, "recall_min_score": 0.42, **config}
    p._client = client or plugin._Client("fixture-token", "http://fixture.invalid/")
    return p


def finish(p):
    p._prefetch_thread.join(2)
    assert not p._prefetch_thread.is_alive()


@pytest.mark.parametrize("query,session", [("unrelated", "session-a"), ("original", "session-b")])
def test_prefetch_never_consumes_another_query_or_session(query, session):
    client = Mock()
    client.search.return_value = [{"content": "private original context"}]
    p = provider(client)
    p.queue_prefetch("original", session_id="session-a")
    finish(p)
    assert p.prefetch(query, session_id=session) == ""
    assert "private original context" in p.prefetch("original", session_id="session-a")


def test_new_generation_does_not_wait_for_or_publish_old_worker():
    started = threading.Event()
    release = threading.Event()
    def search(query, **kwargs):
        if query == "old":
            started.set()
            release.wait(5)
        return [{"content": query}]
    p = provider(Mock(search=search))
    p.queue_prefetch("old")
    old_worker = p._prefetch_thread
    assert started.wait(1)
    try:
        before = time.monotonic()
        p.queue_prefetch("new")
        assert time.monotonic() - before < 0.2
        finish(p)
        release.set()
        old_worker.join(1)
        assert "new" in p.prefetch("new")
        assert p.prefetch("old") == ""
    finally:
        release.set()
        old_worker.join(1)


@pytest.mark.parametrize("transition", ["initialize", "end", "shutdown"])
def test_lifecycle_invalidates_inflight_results(transition, tmp_path):
    started, release = threading.Event(), threading.Event()
    def search(*args, **kwargs):
        started.set()
        release.wait(3)
        return [{"content": "ended session context"}]
    p = provider(Mock(search=search))
    p.queue_prefetch("same")
    worker = p._prefetch_thread
    assert started.wait(1)
    try:
        if transition == "initialize":
            with patch.object(plugin._Client, "health", return_value=True), patch.object(plugin, "_load_config", return_value={}):
                p.initialize("session-a", hermes_home=str(tmp_path))
        elif transition == "end":
            p.on_session_end([])
        else:
            # Release after shutdown's invalidation, without waiting five seconds.
            threading.Timer(0.05, release.set).start()
            p.shutdown()
        release.set()
        worker.join(1)
        assert p.prefetch("same") == ""
    finally:
        release.set()
        worker.join(1)


def test_inflight_work_stays_bounded_under_queue_stress():
    release = threading.Event()
    lock = threading.Lock()
    calls = []
    def search(query, **kwargs):
        with lock:
            calls.append(query)
        release.wait(3)
        return [{"content": query}]
    p = provider(Mock(search=search))
    threads = []
    try:
        for i in range(30):
            p.queue_prefetch(str(i))
            threads.append(p._prefetch_thread)
        time.sleep(0.05)
        assert len(calls) <= 2
        assert len({t for t in threads if t and t.is_alive()}) <= 2
    finally:
        release.set()
        for t in set(threads):
            if t:
                t.join(1)
    assert p.prefetch("29") == ""
    p.queue_prefetch("retry")
    finish(p)
    assert "retry" in p.prefetch("retry")


def test_recall_deadline_includes_time_already_spent_and_rejects_late_results():
    started, release = threading.Event(), threading.Event()
    def search(*args, **kwargs):
        started.set()
        release.wait(3)
        return [{"content": "too late"}]
    p = provider(Mock(search=search), recall_deadline_ms=60)
    p.queue_prefetch("slow")
    assert started.wait(1)
    time.sleep(0.08)
    try:
        before = time.monotonic()
        assert p.prefetch("slow") == ""
        assert time.monotonic() - before < 0.15
    finally:
        release.set()
        finish(p)
    assert p.prefetch("slow") == ""


def test_http_timeout_is_bounded_by_remaining_recall_budget():
    p = provider(recall_deadline_ms=80)
    response = Mock(ok=True)
    response.json.return_value = {"results": []}
    with patch("requests.request", return_value=response) as request:
        p.queue_prefetch("budget")
        finish(p)
    assert 0 < request.call_args.kwargs["timeout"] <= 0.08


def test_worker_keeps_queued_client_and_config_after_session_switch():
    queued = []
    def delayed_spawn(fn, name=""):
        t = spawn(fn, name)
        queued.append(t)
        t.start = lambda: None
        return t
    original = Mock()
    original.search.return_value = []
    p = provider(original)
    with patch.object(plugin, "spawn_context_thread", side_effect=delayed_spawn):
        p.queue_prefetch("queued")
    p._client = Mock()
    p._config = {"recall_top_k": 19, "recall_min_score": 0.9}
    threading.Thread.start(queued[0])
    finish(p)
    assert original.search.call_count == 1
    assert original.search.call_args.kwargs["top_k"] == 3
    assert original.search.call_args.kwargs["min_score"] == 0.42
    p._client.search.assert_not_called()


def test_shutdown_waits_for_all_inflight_workers():
    release = threading.Event()
    def search(*args, **kwargs):
        release.wait(2)
        return []
    p = provider(Mock(search=search))
    p.queue_prefetch("old")
    first = p._prefetch_thread
    p.queue_prefetch("new")
    second = p._prefetch_thread
    # Newest worker finishes first; shutdown must still flush the old one.
    original_join = first.join
    def tracked_join(timeout=None):
        release.set()
        original_join(timeout)
    first.join = Mock(side_effect=tracked_join)
    second.join = lambda timeout=None: None
    try:
        p.shutdown()
        assert first.join.called
    finally:
        release.set()
        original_join(1)
        threading.Thread.join(second, 1)


def test_worker_preserves_hermes_thread_context():
    marker = contextvars.ContextVar("hermes-session", default="missing")
    seen = []
    def search(*args, **kwargs):
        seen.append(marker.get())
        return []
    p = provider(Mock(search=search))
    token = marker.set("queued-session")
    try:
        p.queue_prefetch("context")
    finally:
        marker.reset(token)
    finish(p)
    assert seen == ["queued-session"]


def test_repeated_same_query_does_not_accept_older_generation():
    started, release = threading.Event(), threading.Event()
    calls = []
    def search(*args, **kwargs):
        calls.append(len(calls))
        n = len(calls)
        if n == 1:
            started.set()
            release.wait(2)
        return [{"content": f"generation-{n}"}]
    p = provider(Mock(search=search))
    p.queue_prefetch("same")
    first = p._prefetch_thread
    assert started.wait(1)
    try:
        p.queue_prefetch("same")
        finish(p)
        release.set()
        first.join(1)
        assert "generation-2" in p.prefetch("same")
    finally:
        release.set()
        first.join(1)


def test_session_switch_out_of_order_does_not_publish_previous_session():
    started, release = threading.Event(), threading.Event()
    def search(query, **kwargs):
        started.set()
        release.wait(2)
        return [{"content": "previous session"}]
    p = provider(Mock(search=search))
    p.queue_prefetch("same", session_id="session-a")
    first = p._prefetch_thread
    assert started.wait(1)
    try:
        with patch.object(plugin._Client, "health", return_value=True), patch.object(plugin, "_load_config", return_value={}):
            p.initialize("session-b")
        p._client = Mock()
        p._client.search.return_value = [{"content": "current session"}]
        p.queue_prefetch("same", session_id="session-b")
        finish(p)
        release.set()
        first.join(1)
        assert p.prefetch("same", session_id="session-a") == ""
        assert "current session" in p.prefetch("same", session_id="session-b")
    finally:
        release.set()
        first.join(1)


@pytest.mark.parametrize("disabled", ["auto_recall", "breaker"])
def test_skipped_request_invalidates_previous_cache(disabled):
    client = Mock()
    client.search.return_value = [{"content": "obsolete cache"}]
    p = provider(client)
    p.queue_prefetch("same")
    finish(p)
    if disabled == "auto_recall":
        p._config["auto_recall"] = False
    else:
        p._consecutive_failures = p._breaker_threshold
        p._breaker_open_until = time.monotonic() + 10
    p.queue_prefetch("same")
    assert p.prefetch("same") == ""


def test_prefetch_uses_actual_search_http_contract():
    p = provider()
    response = Mock(ok=True)
    response.json.return_value = {"results": [{"id": "one", "content": "query-specific fact", "score": 0.8}], "total": 1, "limit": 3, "offset": 0}
    with patch("requests.request", return_value=response) as request:
        p.queue_prefetch("specific question", session_id="session-a")
        finish(p)
        block = p.prefetch("specific question", session_id="session-a")
    assert request.call_args.args == ("POST", "http://fixture.invalid/api/v1/memories/search")
    assert request.call_args.kwargs["json"] == {"query": "specific question", "limit": 3, "min_score": 0.42}
    assert request.call_args.kwargs["params"] is None
    assert request.call_args.kwargs["headers"]["Authorization"] == "Bearer fixture-token"
    assert "query-specific fact" in block


def test_initialize_during_client_capture_never_dispatches_old_query(tmp_path):
    entered, release = threading.Event(), threading.Event()
    p = provider(Mock())
    get_client = p._get_client
    errors = []

    def paused_client():
        entered.set()
        assert release.wait(2)
        return get_client()

    def queue():
        try:
            p.queue_prefetch("private profile-a query", session_id="session-a")
        except Exception as exc:
            errors.append(exc)

    with patch.object(p, "_get_client", side_effect=paused_client), patch.object(
        plugin, "_load_config", return_value={"api_key": "profile-b-token"}
    ), patch.object(plugin._Client, "health", return_value=True), patch.object(
        plugin._Client, "search", return_value=[]
    ) as search:
        submitter = threading.Thread(target=queue)
        submitter.start()
        assert entered.wait(1)
        try:
            p.initialize("session-b", hermes_home=str(tmp_path))
        finally:
            release.set()
            submitter.join(2)
        assert not submitter.is_alive()
        assert not errors
        if p._prefetch_thread:
            finish(p)
        search.assert_not_called()
        assert p.prefetch("private profile-a query", session_id="session-a") == ""
        # Obsolete submission must return its bounded worker slot.
        assert p._prefetch_slots.acquire(blocking=False)
        assert p._prefetch_slots.acquire(blocking=False)
        p._prefetch_slots.release()
        p._prefetch_slots.release()


@pytest.mark.parametrize("failure", ["top_k", "min_score", "client", "spawn", "start"])
def test_queue_setup_failure_is_best_effort_and_releases_slots(failure):
    client = Mock(search=Mock(return_value=[{"content": "recovered"}]))
    p = provider(client)
    if failure in ("top_k", "min_score"):
        key = "recall_" + failure
        p._config[key] = "not-a-number"
        p.queue_prefetch("bad setup")
        p._config[key] = 3 if failure == "top_k" else 0.42
    else:
        if failure == "client":
            target, name = p, "_get_client"
            value = {"side_effect": RuntimeError("client setup failed")}
        else:
            target, name = plugin, "spawn_context_thread"
            value = {"side_effect": RuntimeError("spawn failed")}
            if failure == "start":
                thread = spawn(lambda: None)
                thread.start = Mock(side_effect=RuntimeError("start failed"))
                value = {"return_value": thread}
        with patch.object(target, name, **value):
            p.queue_prefetch("bad setup")
    assert p.prefetch("bad setup") == ""
    assert p._prefetch_slots.acquire(blocking=False)
    assert p._prefetch_slots.acquire(blocking=False)
    p._prefetch_slots.release()
    p._prefetch_slots.release()
    p.queue_prefetch("retry")
    finish(p)
    assert "recovered" in p.prefetch("retry")


@pytest.mark.parametrize("response", [
    {"results": {"content": "wrong container"}},
    {"memories": {"content": "wrong container"}},
    [None], ["not a memory"], [{"content": 42}],
    [{"id": "missing-content"}],
    [{"content": "bad priority", "importance": "invalid"}],
    [{"content": "bad score", "score": {}}],
])
def test_malformed_worker_response_is_not_published(response):
    p = provider(Mock(search=Mock(return_value=response)),
                 dedup_injected_memories=True, recall_max_inject_chars=500)
    p.queue_prefetch("malformed")
    finish(p)
    assert p._recall_result == []
    assert p.prefetch("malformed") == ""
    p._client.search.return_value = [{"content": "valid retry"}]
    p.queue_prefetch("valid")
    finish(p)
    assert "valid retry" in p.prefetch("valid")
