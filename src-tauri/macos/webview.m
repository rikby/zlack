#import <AppKit/AppKit.h>
#import <WebKit/WebKit.h>
#import <objc/runtime.h>
#include <stdint.h>

typedef void (^ZlackReply)(id _Nullable, NSString *_Nullable);
typedef bool (*ZlackPolicy)(void *, const char *, bool, bool, bool, bool);
typedef void (*ZlackInvoke)(void *, const char *, const char *, void *);
typedef uint64_t (*ZlackAdvance)(void *);
typedef void (*ZlackDrop)(void *);

static char adapterKey;
static NSString *const handlerName = @"zlack";
static const NSUInteger maxPendingActions = 16;

static BOOL isSlackOrigin(WKSecurityOrigin *origin) {
    return [origin.protocol isEqualToString:@"https"] &&
        [origin.host isEqualToString:@"app.slack.com"] &&
        (origin.port == 0 || origin.port == 443);
}

static BOOL isSlackClient(NSURL *url) {
    if (![url.scheme isEqualToString:@"https"] || ![url.host isEqualToString:@"app.slack.com"] ||
        (url.port && url.port.integerValue != 443) || url.user.length || url.password != nil) return NO;
    NSString *path = url.path;
    return [path isEqualToString:@"/client"] || [path hasPrefix:@"/client/"] ||
        [path isEqualToString:@"/archives"] || [path hasPrefix:@"/archives/"];
}

@interface ZlackWebView : NSObject <WKNavigationDelegate, WKScriptMessageHandlerWithReply>
@property(nonatomic, weak) WKWebView *view;
@property(nonatomic, weak) id<WKNavigationDelegate> downstream;
@property(nonatomic) BOOL signIn;
@property(nonatomic) BOOL provisional;
@property(nonatomic, strong) WKNavigation *provisionalNavigation;
@property(nonatomic, copy) NSString *pendingStatus;
@property(nonatomic, strong) NSMutableArray<NSString *> *pendingActions;
@property(nonatomic) BOOL hasWatched;
@property(nonatomic) BOOL requestWasGet;
@property(nonatomic) uint64_t generation;
@property(nonatomic, copy) ZlackReply watcher;
@property(nonatomic) void *context;
@property(nonatomic) ZlackPolicy policy;
@property(nonatomic) ZlackInvoke invoke;
@property(nonatomic) ZlackAdvance advance;
@property(nonatomic) ZlackDrop dropContext;
@end

@implementation ZlackWebView

- (void)invalidateWatcher {
    ZlackReply reply = self.watcher;
    self.watcher = nil;
    if (reply) reply(nil, @"The Slack document changed");
}

- (void)resetEvents {
    self.hasWatched = NO;
    self.pendingStatus = nil;
    [self.pendingActions removeAllObjects];
    [self invalidateWatcher];
}

- (void)dealloc {
    [self invalidateWatcher];
    self.dropContext(self.context);
}

- (void)webView:(WKWebView *)view
    decidePolicyForNavigationAction:(WKNavigationAction *)action
    decisionHandler:(void (^)(WKNavigationActionPolicy))decision {
    WKFrameInfo *target = action.targetFrame;
    if (action.shouldPerformDownload && (!target || target.mainFrame)) {
        decision(WKNavigationActionPolicyCancel);
        return;
    }

    if (!target || target.mainFrame) {
        NSURLRequest *request = action.request;
        BOOL popup = !target;
        BOOL routed = (!popup || action.sourceFrame.mainFrame) &&
            self.policy(self.context, request.URL.absoluteString.UTF8String,
                [request.HTTPMethod isEqualToString:@"GET"], NO, popup,
                action.navigationType == WKNavigationTypeLinkActivated);
        if (popup || !routed) {
            decision(WKNavigationActionPolicyCancel);
            return;
        }
        self.requestWasGet = [request.HTTPMethod isEqualToString:@"GET"];
    }

    id<WKNavigationDelegate> downstream = self.downstream;
    if ([downstream respondsToSelector:_cmd]) {
        [downstream webView:view decidePolicyForNavigationAction:action decisionHandler:decision];
    } else {
        decision(WKNavigationActionPolicyAllow);
    }
}

- (void)webView:(WKWebView *)view
    decidePolicyForNavigationResponse:(WKNavigationResponse *)response
    decisionHandler:(void (^)(WKNavigationResponsePolicy))decision {
    if (response.forMainFrame && (!response.canShowMIMEType ||
        !self.policy(self.context, response.response.URL.absoluteString.UTF8String, self.requestWasGet, YES, NO, NO))) {
        decision(WKNavigationResponsePolicyCancel);
        return;
    }

    id<WKNavigationDelegate> downstream = self.downstream;
    if ([downstream respondsToSelector:_cmd]) {
        [downstream webView:view decidePolicyForNavigationResponse:response decisionHandler:decision];
    } else {
        decision(WKNavigationResponsePolicyAllow);
    }
}

- (void)webView:(WKWebView *)view didStartProvisionalNavigation:(WKNavigation *)navigation {
    self.provisional = YES;
    self.provisionalNavigation = navigation;
    self.generation = self.advance(self.context);
    [self resetEvents];
    if ([self.downstream respondsToSelector:_cmd]) {
        [self.downstream webView:view didStartProvisionalNavigation:navigation];
    }
}

- (void)webView:(WKWebView *)view didCommitNavigation:(WKNavigation *)navigation {
    if (self.provisionalNavigation == navigation) {
        self.provisional = NO;
        self.provisionalNavigation = nil;
    }
    self.generation = self.advance(self.context);
    [self resetEvents];
    if (self.signIn) {
        view.window.title = [NSString stringWithFormat:@"Zlack Trial sign-in — %@", view.URL.host ?: @""];
    }
    if ([self.downstream respondsToSelector:_cmd]) {
        [self.downstream webView:view didCommitNavigation:navigation];
    }
}

- (void)webView:(WKWebView *)view didFinishNavigation:(WKNavigation *)navigation {
    id<WKNavigationDelegate> downstream = self.downstream;
    if ([downstream respondsToSelector:_cmd]) {
        [downstream webView:view didFinishNavigation:navigation];
    }
}

- (void)webView:(WKWebView *)view navigationAction:(WKNavigationAction *)action
    didBecomeDownload:(WKDownload *)download {
    id<WKNavigationDelegate> downstream = self.downstream;
    if ([downstream respondsToSelector:_cmd]) {
        [downstream webView:view navigationAction:action didBecomeDownload:download];
    }
}

- (void)webView:(WKWebView *)view navigationResponse:(WKNavigationResponse *)response
    didBecomeDownload:(WKDownload *)download {
    id<WKNavigationDelegate> downstream = self.downstream;
    if ([downstream respondsToSelector:_cmd]) {
        [downstream webView:view navigationResponse:response didBecomeDownload:download];
    }
}

- (void)webView:(WKWebView *)view didFailProvisionalNavigation:(WKNavigation *)navigation
    withError:(NSError *)error {
    if (self.provisionalNavigation == navigation) {
        self.provisional = NO;
        self.provisionalNavigation = nil;
    }
    if ([self.downstream respondsToSelector:_cmd]) {
        [self.downstream webView:view didFailProvisionalNavigation:navigation withError:error];
    }
}

- (void)webViewWebContentProcessDidTerminate:(WKWebView *)view {
    self.provisional = YES;
    self.generation = self.advance(self.context);
    [self resetEvents];
    if ([self.downstream respondsToSelector:_cmd]) {
        [self.downstream webViewWebContentProcessDidTerminate:view];
    }
}

- (void)userContentController:(WKUserContentController *)controller
    didReceiveScriptMessage:(WKScriptMessage *)message replyHandler:(ZlackReply)reply {
    if (self.signIn || self.provisional || message.webView != self.view ||
        !message.frameInfo.mainFrame || !isSlackOrigin(message.frameInfo.securityOrigin) ||
        !isSlackClient(message.frameInfo.request.URL) || !isSlackClient(self.view.URL)) {
        reply(nil, @"Slack bridge unavailable for this document");
        return;
    }
    if (![message.body isKindOfClass:[NSString class]] || [message.body length] > 359 * 1024 * 1024) {
        reply(nil, @"Invalid Slack bridge request");
        return;
    }
    NSString *body = message.body;
    if ([body isEqualToString:@"watch"]) {
        [self invalidateWatcher];
        self.hasWatched = YES;
        NSString *action = self.pendingActions.firstObject;
        if (action) {
            [self.pendingActions removeObjectAtIndex:0];
            reply(action, nil);
        } else if (self.pendingStatus) {
            NSString *status = self.pendingStatus;
            self.pendingStatus = nil;
            reply(status, nil);
        } else {
            self.watcher = reply;
        }
        return;
    }
    self.invoke(self.context, message.frameInfo.request.URL.absoluteString.UTF8String,
        body.UTF8String, (__bridge_retained void *)[reply copy]);
}
@end

bool zlack_supported_webkit(void) {
    if (@available(macOS 14.0, *)) return true;
    return false;
}

bool zlack_install_webview(void *handle, bool signIn, void *context, ZlackPolicy policy,
    ZlackInvoke invoke, ZlackAdvance advance, ZlackDrop dropContext) {
    WKWebView *view = (__bridge WKWebView *)handle;
    if (!view.navigationDelegate || objc_getAssociatedObject(view, &adapterKey)) return false;
    ZlackWebView *adapter = [ZlackWebView new];
    adapter.view = view;
    adapter.downstream = view.navigationDelegate;
    adapter.signIn = signIn;
    adapter.pendingActions = [NSMutableArray array];
    adapter.context = context;
    adapter.policy = policy;
    adapter.invoke = invoke;
    adapter.advance = advance;
    adapter.dropContext = dropContext;

    WKUserContentController *controller = view.configuration.userContentController;
    [controller removeAllScriptMessageHandlers];
    if (signIn) {
        [controller removeAllUserScripts];
    } else {
        [controller addScriptMessageHandlerWithReply:adapter contentWorld:WKContentWorld.pageWorld name:handlerName];
    }

    // WebKit's navigation delegate is weak. Wry continues to own the forwarded delegate.
    objc_setAssociatedObject(view, &adapterKey, adapter, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    view.navigationDelegate = adapter;
    return true;
}

void zlack_reply(void *handle, const char *payload) {
    ZlackReply reply = (__bridge_transfer ZlackReply)handle;
    NSString *value = payload ? [NSString stringWithUTF8String:payload] : nil;
    dispatch_async(dispatch_get_main_queue(), ^{
        reply(value, value ? nil : @"Slack bridge response unavailable");
    });
}

void zlack_dispatch(void *task, void (*run)(void *)) {
    dispatch_async(dispatch_get_main_queue(), ^{ run(task); });
}

bool zlack_load_url(void *handle, const char *url) {
    WKWebView *view = (__bridge WKWebView *)handle;
    NSURL *target = [NSURL URLWithString:[NSString stringWithUTF8String:url]];
    return target && [view loadRequest:[NSURLRequest requestWithURL:target]] != nil;
}

void zlack_recover_blank_view(void *handle, const char *url) {
    WKWebView *view = (__bridge WKWebView *)handle;
    if (!view.URL || [view.URL.absoluteString isEqualToString:@"about:blank"]) {
        zlack_load_url(handle, url);
    }
}

void zlack_send_page_event(void *handle, uint64_t generation, const char *payload,
    bool isStatus, const char *fallback) {
    WKWebView *view = (__bridge WKWebView *)handle;
    ZlackWebView *adapter = objc_getAssociatedObject(view, &adapterKey);
    if (!adapter || adapter.signIn || adapter.provisional || adapter.generation != generation ||
        !isSlackClient(view.URL)) return;
    NSString *event = [NSString stringWithUTF8String:payload];
    ZlackReply reply = adapter.watcher;
    adapter.watcher = nil;
    if (reply) {
        reply(event, nil);
    } else if (isStatus) {
        adapter.pendingStatus = event;
    } else if (adapter.hasWatched) {
        // A reply may have just consumed the watcher; keep action order until
        // its document immediately watches again. Status has its own latest slot.
        if (adapter.pendingActions.count == maxPendingActions) {
            [adapter.pendingActions removeObjectAtIndex:0];
        }
        [adapter.pendingActions addObject:event];
    } else if (fallback && ![view.URL.absoluteString isEqualToString:[NSString stringWithUTF8String:fallback]]) {
        zlack_load_url(handle, fallback);
    }
}
