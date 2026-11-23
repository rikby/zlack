const assert = require('node:assert/strict');
const test = require('node:test');
const { createLimiter, readBlob, isDownloadUrl } = require('../src-tauri/bounded-download.cjs');

const slackFile = 'https://files.slack.com/files-pri/file/download/report.pdf';

function response(chunks, { url = slackFile, length } = {}) {
    let offset = 0;
    let canceled = false;
    const stream = new ReadableStream({
        pull(controller) {
            if (offset === chunks.length) controller.close();
            else controller.enqueue(Uint8Array.from(chunks[offset++]));
        },
        cancel() { canceled = true; },
    }, { highWaterMark: 0 });
    return {
        url,
        ok: true,
        headers: new Headers({
            'content-type': 'application/pdf',
            ...(length === undefined ? {} : { 'content-length': length }),
        }),
        body: stream,
        reads: () => offset,
        canceled: () => canceled,
    };
}

test('reads an authenticated Slack attachment up to its exact byte limit', async () => {
    const source = response([[1, 2], [3, 4]]);
    const file = await readBlob(source, 4, () => {});
    assert.equal(file.type, 'application/pdf');
    assert.deepEqual(new Uint8Array(await file.arrayBuffer()), Uint8Array.of(1, 2, 3, 4));
});

test('declared oversized or unapproved responses are canceled before reading', async () => {
    for (const source of [
        response([[1], [2]], { length: '200' }),
        response([[1], [2]], { url: 'https://example.test/report.pdf' }),
    ]) {
        await assert.rejects(readBlob(source, 4, () => {}));
        assert.equal(source.reads(), 0);
        assert.equal(source.canceled(), true);
    }
});

test('stops immediately when a chunk would exceed the budget', async () => {
    const source = response([[1, 2, 3], [4, 5], [6]], { length: '3' });
    await assert.rejects(readBlob(source, 4, () => {}), /too large/i);
    assert.equal(source.reads(), 2);
    assert.equal(source.canceled(), true);
});

test('accepts Slack CDN and app Blob URLs and rejects unrelated origins', () => {
    assert.equal(isDownloadUrl('https://files.slack-edge.com/file'), true);
    assert.equal(isDownloadUrl('https://slack-imgs.com/?url=example'), true);
    assert.equal(isDownloadUrl('https://example.slack-files.com/file'), true);
    assert.equal(isDownloadUrl('blob:https://app.slack.com/attachment'), true);
    assert.equal(isDownloadUrl('https://files.slack.com.example.test/file'), false);
    assert.equal(isDownloadUrl('https://files.slack.com:8443/file'), false);
});

test('queues ordinary consecutive saves and bounds the waiting queue', async () => {
    const run = createLimiter({ intervalMs: 0, maxWaiting: 1 });
    let finishFirst;
    const first = run(() => new Promise(resolve => { finishFirst = resolve; }));
    const second = run(() => 'second saved');
    await assert.rejects(run(() => 'extra save'), /queue is full/i);
    finishFirst('first saved');
    assert.deepEqual(await Promise.all([first, second]), ['first saved', 'second saved']);
    assert.equal(await run(() => 'next saved'), 'next saved');
});

test('aborts a transfer that stops delivering bytes and frees the slot', async () => {
    const run = createLimiter({ idleMs: 10, totalMs: 1000, intervalMs: 0 });
    await assert.rejects(run(signal => new Promise((_, reject) => {
        signal.addEventListener('abort', () => reject(signal.reason));
    })), { name: 'AbortError' });
    assert.equal(await run(() => 'next saved'), 'next saved');
});
