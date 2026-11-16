"""Offline checkpoint/consent/privacy boundary regressions."""
import hashlib
import json
from unittest.mock import Mock, patch

import pytest
from test_prefetch import plugin, provider

BUDGET = plugin._WEBHOOK_CHUNK_BYTES


def immediate(fn, name=""):
    return Mock(start=fn)


def receipts():
    """Mock ingest_conversation returning unique, receipt-shaped ids."""
    client = Mock()
    client.ingest_conversation = Mock(side_effect=lambda **kw: (
        'receipt-' + str(client.ingest_conversation.call_count)))
    return client


def writes(client):
    return [call.kwargs['summary'] for call in client.ingest_conversation.call_args_list]


def stored(client):
    """Mock fetch(id) over what the mock client received (no server framing)."""
    bodies = writes(client)
    return lambda receipt: bodies[int(receipt.rsplit('-', 1)[1]) - 1]


def assert_within_budget(text):
    """Every webhook write is one embedding input: 8191 tokens at most.

    8000 bytes is safe under both the densest realistic encodings: CJK runs
    ~1 token/character (3 bytes) and base64/hex ~2 characters per token.
    """
    assert len(text.encode('utf-8')) <= BUDGET
    assert len(text) <= 8191  # worst case: 1 token per character


@pytest.mark.parametrize("response", [None, False, True, "", {}, {"ok": True}])
def test_required_checkpoint_rejects_missing_receipt(response):
    p = provider(Mock(ingest_conversation=Mock(return_value=response)))
    with pytest.raises(RuntimeError):
        p.on_pre_compress([{"role": "user", "content": "important evidence"}], require_checkpoint=True)
    assert p._consecutive_failures == 1


def test_real_client_transport_failure_blocks_checkpoint():
    c = plugin._Client("fixture", "http://fixture.invalid")
    c.request = Mock(side_effect=ConnectionError("offline"))
    p = provider(c)
    with pytest.raises(RuntimeError):
        p.on_pre_compress([{"role": "user", "content": "evidence"}], require_checkpoint=True)
    assert p._consecutive_failures == 1


def test_complete_chunked_evidence_and_manifest():
    c = Mock(ingest_conversation=Mock(side_effect=lambda **kw: 'receipt-' + str(c.ingest_conversation.call_count)))
    p = provider(c)
    messages = [{"role": "user", "content": "é" * 40000 + "IMPORTANT_END"},
                {"role": "assistant", "content": [{"type": "text", "text": "STRUCTURED"}],
                 "tool_calls": [{"id": "call-1", "function": {"arguments": "TOOL_ARGS"}}]},
                {"role": "tool", "tool_call_id": "call-1", "content": "IMPORTANT_TOOL_RESULT"}]
    marker = p.on_pre_compress(messages, require_checkpoint=True)
    writes = [x.kwargs['summary'] for x in c.ingest_conversation.call_args_list]
    assert len(writes) > 2
    assert all(len(x.encode('utf-8')) < 64000 for x in writes)
    manifest = json.loads(writes[-1])
    assert manifest['format'] == 'memex8-transcript-manifest-v1'
    assert marker == 'checkpoint: receipt-' + str(len(writes))
    assert json.loads(''.join(writes[:-1])) == messages
    assert len(manifest['chunks']) == len(writes) - 1
    import hashlib
    assert manifest['sha256'] == hashlib.sha256(''.join(writes[:-1]).encode('utf-8')).hexdigest()
    for part, entry in zip(writes[:-1], manifest['chunks']):
        assert entry['bytes'] == len(part.encode('utf-8'))
        assert entry['sha256'] == hashlib.sha256(part.encode('utf-8')).hexdigest()


@pytest.mark.parametrize('hook', ['on_pre_compress', 'on_session_end'])
def test_redacts_complete_ordered_messages_before_filtering(hook):
    c = Mock(ingest_conversation=Mock(return_value='receipt'))
    p = provider(c)
    messages = [{'role': 'tool', 'content': 'x' * 501 + '<PRIVATE>SECRET_START'},
                {'role': 'user', 'content': [{'text': 'SECRET_MIDDLE'}]},
                {'role': 'assistant', 'content': 'SECRET_END</private> visible'}]
    with patch.object(plugin, 'spawn_context_thread', immediate):
        getattr(p, hook)(messages)
    writes = str(c.ingest_conversation.call_args_list)
    assert c.ingest_conversation.called
    assert 'SECRET_' not in writes
    assert 'visible' in writes


@pytest.mark.parametrize('setting', [False, 'false', 'False', 0])
def test_auto_consent_all_optional_paths(setting):
    c = Mock(ingest_conversation=Mock(return_value='receipt'))
    p = provider(c, auto_sync=setting)
    with patch.object(plugin, 'spawn_context_thread', immediate):
        p.sync_turn('meaningful user request', 'answer')
        p.on_session_end([{'role': 'user', 'content': 'evidence'}])
        assert p.on_pre_compress([{'role': 'user', 'content': 'evidence'}]) == ''
    assert not c.store.called
    assert not c.ingest_conversation.called
    assert p.on_pre_compress([{'role': 'user', 'content': 'evidence'}], require_checkpoint=True).startswith('checkpoint:')


def test_sync_redaction_spans_filtered_turns_and_later_calls():
    c = Mock()
    p = provider(c)
    with patch.object(plugin, 'spawn_context_thread', immediate):
        p.sync_turn('ok', 'x' * 501 + '<private>SECRET_START')
        p.sync_turn('SECRET_USER with enough characters to store',
                    'SECRET_ASSISTANT</private> public answer')
        p.sync_turn('a sufficiently long public question', 'public answer')
    assert 'SECRET_' not in str(c.store.call_args_list)
    assert c.store.called


@pytest.mark.parametrize('fail_at', [2, 4])
def test_partial_chunk_or_manifest_failure_never_succeeds(fail_at):
    c = Mock()
    c.ingest_conversation.side_effect = lambda **kw: None if c.ingest_conversation.call_count == fail_at else 'receipt'
    p = provider(c)
    with pytest.raises(RuntimeError):
        p.on_pre_compress([{'role': 'tool', 'content': 'a' * 70000}], require_checkpoint=True)
    assert p._consecutive_failures == 1


@pytest.mark.parametrize('response', [{'ok': True}, {'id': True}, {'id': {}}, {'id': ''}, {'id': 'bad\nreceipt'}])
def test_client_rejects_non_receipts(response):
    c = plugin._Client('fixture', 'http://fixture.invalid')
    c.request = Mock(return_value=response)
    assert c.ingest_conversation('evidence') is None


def test_optional_failure_records_failure_without_marker():
    p = provider(Mock(ingest_conversation=Mock(return_value=None)))
    assert p.on_pre_compress([{'role': 'tool', 'content': 'evidence'}]) == ''
    assert p._consecutive_failures == 1


def test_nested_private_spans_structured_fields_and_unclosed_tail():
    messages = [{'role': 'user', 'content': [
        {'text': 'public<PRIVATE>SECRET<private>NESTED</private>'},
        {'text': 'HIDDEN</PrIvAtE> visible<private>TAIL'}]},
        {'role': 'tool', 'content': 'LAST_SECRET'}]
    text = plugin._transcript(messages)
    assert all(secret not in text for secret in ['SECRET', 'NESTED', 'HIDDEN', 'TAIL'])
    assert 'public' in text and 'visible' in text
    assert isinstance(json.loads(text), list)


def test_manifest_tree_bounds_large_conversation():
    c = receipts()
    p = provider(c)
    text = plugin._transcript([{'role': 'tool', 'content': 'z' * 1600000}])
    root_id = p._archive_transcript(text, 'test')
    bodies = writes(c)
    root = json.loads(bodies[int(root_id.rsplit('-', 1)[1]) - 1])
    assert root['level'] == 1
    assert len(root['chunks']) > 1
    for body in bodies:
        assert_within_budget(body)
    # Full multi-level reassembly, hash-verified at every node.
    assert plugin.reassemble_transcript(root, stored(c)) == text


def test_dense_24kb_payload_never_exceeds_one_embedding_input():
    """A realistic dense 24KB window used to be embedded as ~24KB single inputs.

    That is ~8000 tokens for CJK (3 bytes/char) and a long way past the 8191
    limit for base64 tool output: the webhook 400s, no receipt comes back, and
    a required checkpoint blocks compression forever.
    """
    c = receipts()
    p = provider(c)
    messages = [
        {'role': 'user', 'content': '分析这个部署记录 <private>APIKEY_hidden</private>'},
        {'role': 'tool', 'tool_call_id': 'call-1', 'content': {
            'argv': ['--flag', '--token=' + 'A' * 300],
            'stdout': '界' * 2000,                       # ~6KB of CJK
            'blob': hashlib.sha256(b'x').hexdigest() * 40,  # ~2.5KB of hex
            'path': '/srv/' + 'deeply/nested/segment/' * 30,
        }},
        {'role': 'assistant', 'content': [{'type': 'text', 'text': '結果' * 1000}]},
        {'role': 'user', 'content': '继续' * 800 + ' END_MARKER'},
    ]
    summary = plugin._transcript(messages)
    assert 20000 <= len(summary.encode('utf-8')) <= 30000  # the reviewed size
    p.on_pre_compress(messages, require_checkpoint=True)
    bodies = writes(c)
    assert len(bodies) > 2
    for body in bodies:
        assert_within_budget(body)
    assert 'APIKEY_hidden' not in ''.join(bodies)
    assert plugin.reassemble_transcript(json.loads(bodies[-1]), stored(c)) == summary


def test_window_starting_inside_private_span_stays_redacted():
    """Compression rotates the session, so the next window can start mid-span.

    A fresh redactor sees only the closing tag and archives the span's text.
    """
    c = receipts()
    p = provider(c)
    p.on_pre_compress([
        {'role': 'user', 'content': 'public opening <private>SECRET_IN_SPAN'},
        {'role': 'assistant', 'content': 'SECRET_STILL_IN_SPAN'},
    ], require_checkpoint=True)
    # Post-compression window: begins inside the span, closes later.
    p.on_pre_compress([
        {'role': 'user', 'content': 'SECRET_INSIDE_WINDOW</private> visible after close'},
        {'role': 'assistant', 'content': 'ordinary answer'},
    ], require_checkpoint=True)
    archived = ''.join(writes(c))
    assert 'SECRET_' not in archived
    assert 'visible after close' in archived and 'ordinary answer' in archived


def test_cumulative_resend_of_a_window_is_not_redacted_twice():
    c = receipts()
    p = provider(c)
    first = [{'role': 'user', 'content': 'IMPORTANT_HEAD <private>SEED'}]
    p.on_pre_compress(first, require_checkpoint=True)  # ends inside the span
    # Same leading message re-sent with the span still open: the head must not
    # be swallowed by the depth carried over from the previous archive.
    resent = first + [{'role': 'tool', 'content': 'PRIVATE_TAIL</private> public tail'}]
    p.on_pre_compress(resent, require_checkpoint=True)
    # Second archive only: the re-sent head survives, text still inside the
    # span does not, and text after the closing tag does.
    second = ''.join(writes(c)[2:])
    assert 'IMPORTANT_HEAD' in second and 'public tail' in second
    assert 'PRIVATE_TAIL' not in second and 'SEED' not in second


def test_empty_transcript_refuses_instead_of_looping():
    c = receipts()
    p = provider(c)
    with pytest.raises(RuntimeError, match='empty transcript'):
        p._archive_transcript('', 'test')
    assert not c.ingest_conversation.called
    # Unreachable in practice (transcripts are JSON), but the loop must not
    # depend on the payload being non-empty to terminate.
    p._archive_transcript(' ', 'test')
    assert writes(c)[0] == ' '
    assert json.loads(writes(c)[-1])['chunks'][0]['bytes'] == 1


def test_manifest_records_framing_and_rejects_tampered_chunks():
    c = receipts()
    p = provider(c)
    messages = [{'role': 'tool', 'content': 'y' * 20000}]
    p.on_pre_compress(messages, require_checkpoint=True)
    bodies = writes(c)
    page = json.loads(bodies[-1])
    assert page['framing'] == {
        'prefix': plugin._WEBHOOK_FRAMING_PREFIX,
        'suffix': plugin._WEBHOOK_FRAMING_SUFFIX,
    }
    # A server that stores the chunk inside the documented wrapper reassembles
    # exactly; the framing is stripped from what the manifest recorded.
    framed = lambda receipt: (plugin._WEBHOOK_FRAMING_PREFIX
                             + bodies[int(receipt.rsplit('-', 1)[1]) - 1]
                             + plugin._WEBHOOK_FRAMING_SUFFIX)
    assert plugin.reassemble_transcript(page, framed) == plugin._transcript(messages)
    def tampered(receipt):
        body = bodies[int(receipt.rsplit('-', 1)[1]) - 1]
        return plugin._WEBHOOK_FRAMING_PREFIX + body.replace('y', 'z', 1) + plugin._WEBHOOK_FRAMING_SUFFIX
    with pytest.raises(ValueError, match='recorded hash'):
        plugin.reassemble_transcript(page, tampered)
