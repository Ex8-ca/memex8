"""Offline checkpoint/consent/privacy boundary regressions."""
import json
from unittest.mock import Mock, patch

import pytest
from test_prefetch import plugin, provider


def immediate(fn, name=""):
    return Mock(start=fn)


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
    c = Mock()
    c.ingest_conversation.side_effect = lambda **kw: 'id-' + str(c.ingest_conversation.call_count)
    p = provider(c)
    text = plugin._transcript([{'role': 'tool', 'content': 'z' * 1600000}])
    p._archive_transcript(text, 'test')
    writes = [call.kwargs['summary'] for call in c.ingest_conversation.call_args_list]
    root = json.loads(writes[-1])
    assert root['level'] == 1
    assert len(root['chunks']) == 2
    assert all(len(value.encode('utf-8')) < 64000 for value in writes)
    pages = [json.loads(writes[int(node['id'][3:]) - 1]) for node in root['chunks']]
    parts = [writes[int(chunk['id'][3:]) - 1] for page in pages for chunk in page['chunks']]
    assert ''.join(parts) == text
