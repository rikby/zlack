const { execFileSync } = require('node:child_process');
const { mkdtempSync, rmSync } = require('node:fs');
const { tmpdir } = require('node:os');
const path = require('node:path');
const test = require('node:test');

test('macOS adapter preserves delegates and binds replies to the current document', {
    skip: process.platform !== 'darwin',
}, () => {
    const directory = mkdtempSync(path.join(tmpdir(), 'zlack-webview-test-'));
    try {
        const output = path.join(directory, 'webview-test');
        execFileSync('xcrun', [
            'clang', '-fobjc-arc', '-mmacosx-version-min=14.0',
            path.resolve(__dirname, 'macos-webview-adapter.test.m'),
            path.resolve(__dirname, '../src-tauri/macos/webview.m'),
            '-framework', 'AppKit', '-framework', 'WebKit', '-o', output,
        ]);
        execFileSync(output);
    } finally {
        rmSync(directory, { recursive: true, force: true });
    }
});
