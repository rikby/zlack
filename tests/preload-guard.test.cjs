const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const read = file => fs.readFileSync(path.join(__dirname, '..', 'src-tauri', file), 'utf8');
const guard = read('preload-guard.js');
const entrypoint = [
    "(() => { 'use strict'; if (!(",
    guard,
    ')) return;',
    ...[
        'notification-bridge.cjs', 'context-menu-bridge.cjs', 'download-names.cjs',
        'slack-navigation.cjs', 'bounded-download.cjs', 'preload.js',
    ].map(read),
    '})();',
].join('\n');

test('runs only in top-level documents on the Slack app origin and client paths', () => {
    for (const href of [
        'https://app.slack.com/client', 'https://app.slack.com/client/T0123456/C0123456',
        'https://app.slack.com/archives/C0123456',
    ]) {
        const window = { location: new URL(href) };
        window.top = window;
        assert.equal(vm.runInNewContext(guard, { window }), true);
    }
});

test('the full preload installs its hooks and starts watching from a trusted client page', () => {
    const listeners = new Set();
    const page = {
        location: new URL('https://app.slack.com/client/T0123456/C0123456'),
        navigator: { permissions: { query() {} }, sendBeacon() {} },
        document: { readyState: 'loading', addEventListener(type) { listeners.add(type); } },
        addEventListener(type) { listeners.add(type); },
        XMLHttpRequest: class { open() {} setRequestHeader() {} send() {} },
        URL,
        EventTarget,
        setTimeout() {},
        clearTimeout() {},
        webkit: { messageHandlers: {
            zlack: { postMessage(command) {
                assert.equal(command, 'watch');
                listeners.add('native watch');
                return new Promise(() => {});
            } },
        } },
    };
    page.window = page;
    page.top = page;
    const originalFetch = () => {};
    const originalOpen = () => {};
    page.fetch = originalFetch;
    page.open = originalOpen;
    vm.runInNewContext(entrypoint, page);
    assert.notEqual(page.fetch, originalFetch);
    assert.notEqual(page.open, originalOpen);
    assert.equal(typeof page.Notification, 'function');
    assert.equal(typeof page.navigator.serviceWorker.register, 'function');
    assert.ok(listeners.has('DOMContentLoaded'));
    assert.ok(listeners.has('contextmenu'));
    assert.ok(listeners.has('native watch'));
});

test('installs no Slack hooks or module globals at sign-in, external, offline or iframe pages', () => {
    for (const [href, isTop] of [
        ['https://app.slack.com/workspace-signin', true],
        ['https://team.slack.com/client/T0123456', true],
        ['https://identity.example.test/signin', true],
        ['https://app.slack.com:8443/client', true],
        ['https://app.slack.com.example.test/client', true],
        ['tauri://localhost/index.html', true],
        ['https://app.slack.com/client/T0123456', false],
    ]) {
        const window = { location: new URL(href) };
        window.top = isTop ? window : {};
        const before = Object.keys(window);
        const context = { window, get navigator() { assert.fail('The preload accessed navigator'); } };
        vm.runInNewContext(entrypoint, context);
        assert.deepEqual(Object.keys(window), before);
        assert.equal(context.__ZlackNotificationBridge, undefined);
    }
});

test('only the workspace window labels have app IPC capability', () => {
    const capability = JSON.parse(read('capabilities/slack.json'));
    assert.equal(capability.local, false);
    assert.deepEqual(capability.windows, ['main', 'workspace-*']);
    assert.deepEqual(capability.remote.urls, ['https://app.slack.com/*']);
});
