// Keep the wire size limits in sync with downloads.rs.
const ZlackDownloads = (() => {
    const fileLimit = 256 * 1024 * 1024;
    const imageLimit = 64 * 1024 * 1024;

    function isDownloadUrl(value) {
        try {
            const url = new URL(value);
            if (url.protocol === 'blob:') return url.origin === 'https://app.slack.com';
            if (url.protocol !== 'https:' || url.port || url.username || url.password) return false;
            return url.hostname === 'slack-imgs.com'
                || ['slack.com', 'slack-edge.com', 'slack-files.com'].some(
                    domain => url.hostname === domain || url.hostname.endsWith('.' + domain)
                );
        } catch (_) {
            return false;
        }
    }

    // The slot spans fetch, reading and saving. Pending entries retain no file bytes.
    function createLimiter({
        idleMs = 30_000, totalMs = 600_000, intervalMs = 300, maxWaiting = 8,
    } = {}) {
        let active = false;
        let lastFinish = -Infinity;
        const waiting = [];
        async function drain() {
            if (active || !waiting.length) return;
            active = true;
            const { transfer, resolve, reject } = waiting.shift();
            const delay = intervalMs - (performance.now() - lastFinish);
            if (delay > 0) await new Promise(done => setTimeout(done, delay));
            const controller = new AbortController();
            let idle;
            let total;
            const keepReading = () => {
                clearTimeout(idle);
                idle = setTimeout(() => controller.abort(), idleMs);
            };
            const doneReading = () => {
                clearTimeout(idle);
                clearTimeout(total);
            };
            try {
                total = setTimeout(() => controller.abort(), totalMs);
                keepReading();
                resolve(await transfer(controller.signal, keepReading, doneReading));
            } catch (error) {
                reject(error);
            } finally {
                doneReading();
                // Spacing fetch starts alone can bunch up consecutive writes
                // when a slow fetch is followed by a cached response.
                lastFinish = performance.now();
                active = false;
                void drain();
            }
        }
        return function run(transfer) {
            if (active && waiting.length >= maxWaiting) {
                return Promise.reject(new Error('The download queue is full. Try again shortly'));
            }
            return new Promise((resolve, reject) => {
                waiting.push({ transfer, resolve, reject });
                void drain();
            });
        };
    }

    async function readBlob(response, limit, keepReading) {
        const rejectBody = (message) => {
            if (response.body) void response.body.cancel().catch(() => {});
            throw new Error(message);
        };
        if (!isDownloadUrl(response.url)) rejectBody('Unexpected download destination');
        if (!response.ok) rejectBody('HTTP ' + response.status);
        if (Number(response.headers.get('content-length')) > limit) {
            rejectBody('File is too large to download');
        }
        if (!response.body) throw new Error('Download stream is unavailable');

        const reader = response.body.getReader();
        const chunks = [];
        let total = 0;
        try {
            keepReading();
            for (;;) {
                const { done, value } = await reader.read();
                if (done) break;
                if (value.byteLength > limit - total) {
                    void reader.cancel().catch(() => {});
                    throw new Error('File is too large to download');
                }
                keepReading();
                total += value.byteLength;
                chunks.push(value);
            }
        } finally {
            reader.releaseLock();
        }
        if (!total) throw new Error('Empty download');
        return new Blob(chunks, { type: response.headers.get('content-type') || '' });
    }

    return { fileLimit, imageLimit, isDownloadUrl, createLimiter, readBlob };
})();

if (typeof module !== 'undefined' && module.exports) module.exports = ZlackDownloads;
