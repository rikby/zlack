#import <AppKit/AppKit.h>
#import <WebKit/WebKit.h>
#include <assert.h>

typedef bool (*Policy)(void *, const char *, bool, bool, bool, bool);
typedef void (*Invoke)(void *, const char *, const char *, void *);
typedef uint64_t (*Advance)(void *);
typedef void (*Drop)(void *);
extern bool zlack_install_webview(void *, bool, void *, Policy, Invoke, Advance, Drop);
extern void zlack_send_page_event(void *, uint64_t, const char *, bool, const char *);

@interface TestController : NSObject
@property(nonatomic) BOOL scriptsRemoved;
@property(nonatomic) BOOL handlersRemoved;
@property(nonatomic, strong) id<WKScriptMessageHandlerWithReply> handler;
@end
@implementation TestController
- (id)userContentController { return self; }
- (void)removeAllUserScripts { self.scriptsRemoved = YES; }
- (void)removeAllScriptMessageHandlers { self.handlersRemoved = YES; }
- (void)addScriptMessageHandlerWithReply:(id)handler contentWorld:(WKContentWorld *)world
    name:(NSString *)name {
    assert([name isEqualToString:@"zlack"]);
    self.handler = handler;
}
@end

@interface TestView : NSObject
@property(nonatomic, weak) id<WKNavigationDelegate> navigationDelegate;
@property(nonatomic, strong) TestController *configuration;
@property(nonatomic, strong) NSURL *URL;
@property(nonatomic, strong) NSWindow *window;
@end
@implementation TestView
@end

@interface TestFrame : NSObject
@property(nonatomic, getter=isMainFrame) BOOL mainFrame;
@property(nonatomic, strong) NSURLRequest *request;
@end
@implementation TestFrame
- (id)securityOrigin { return self; }
- (NSString *)protocol { return self.request.URL.scheme; }
- (NSString *)host { return self.request.URL.host; }
- (NSInteger)port { return self.request.URL.port.integerValue; }
@end

@interface TestNavigation : NSObject
@property(nonatomic, strong) TestFrame *targetFrame;
@property(nonatomic, strong) TestFrame *sourceFrame;
@property(nonatomic, strong) NSURLRequest *request;
@property(nonatomic) BOOL shouldPerformDownload;
@property(nonatomic) WKNavigationType navigationType;
@property(nonatomic, getter=isForMainFrame) BOOL forMainFrame;
@property(nonatomic) BOOL canShowMIMEType;
@property(nonatomic, strong) NSURLResponse *response;
@end
@implementation TestNavigation
@end

@interface TestMessage : NSObject
@property(nonatomic, strong) TestView *webView;
@property(nonatomic, strong) TestFrame *frameInfo;
@property(nonatomic, strong) NSString *body;
@end
@implementation TestMessage
@end

@interface Downstream : NSObject <WKNavigationDelegate>
@property(nonatomic) unsigned commits;
@property(nonatomic) unsigned finishes;
@end
@implementation Downstream
- (void)webView:(WKWebView *)view decidePolicyForNavigationAction:(WKNavigationAction *)action
    decisionHandler:(void (^)(WKNavigationActionPolicy))decision {
    decision(WKNavigationActionPolicyAllow);
}
- (void)webView:(WKWebView *)view decidePolicyForNavigationResponse:(WKNavigationResponse *)response
    decisionHandler:(void (^)(WKNavigationResponsePolicy))decision {
    decision(WKNavigationResponsePolicyAllow);
}
- (void)webView:(WKWebView *)view didCommitNavigation:(WKNavigation *)navigation { self.commits++; }
- (void)webView:(WKWebView *)view didFinishNavigation:(WKNavigation *)navigation { self.finishes++; }
@end

typedef struct {
    uint64_t generation;
    unsigned actions;
    unsigned responses;
} TestContext;

static bool policy(void *context, const char *url, bool get, bool response, bool popup, bool link) {
    TestContext *state = context;
    if (response) state->responses++;
    else state->actions++;
    return strcmp(url, "https://app.slack.com/client") == 0;
}
static void invoke(void *context, const char *url, const char *body, void *reply) { assert(false); }
static uint64_t advance(void *context) { return ++((TestContext *)context)->generation; }
static void dropContext(void *context) {}

int main(void) {
    @autoreleasepool {
        TestContext state = {0};
        Downstream *downstream = [Downstream new];
        TestView *view = [TestView new];
        view.navigationDelegate = downstream;
        view.configuration = [TestController new];
        view.URL = [NSURL URLWithString:@"https://app.slack.com/client"];
        assert(zlack_install_webview((__bridge void *)view, false, &state, policy, invoke, advance, dropContext));
        assert(view.configuration.handlersRemoved && !view.configuration.scriptsRemoved);
        assert(view.configuration.handler != nil);

        // Main-frame navigation checks the action and the final response.
        id<WKNavigationDelegate> adapter = view.navigationDelegate;
        TestFrame *top = [TestFrame new];
        top.mainFrame = YES;
        top.request = [NSURLRequest requestWithURL:view.URL];
        TestNavigation *request = [TestNavigation new];
        request.targetFrame = top;
        request.sourceFrame = top;
        request.request = top.request;
        __block unsigned decisions = 0;
        [adapter webView:(id)view decidePolicyForNavigationAction:(id)request
            decisionHandler:^(WKNavigationActionPolicy decision) {
                assert(decision == WKNavigationActionPolicyAllow);
                decisions++;
            }];
        request.forMainFrame = YES;
        request.canShowMIMEType = YES;
        NSURL *external = [NSURL URLWithString:@"https://identity.example.test/"];
        request.response = [[NSURLResponse alloc] initWithURL:external MIMEType:@"text/html"
            expectedContentLength:0 textEncodingName:nil];
        [adapter webView:(id)view decidePolicyForNavigationResponse:(id)request
            decisionHandler:^(WKNavigationResponsePolicy decision) {
                assert(decision == WKNavigationResponsePolicyCancel);
                decisions++;
            }];
        assert(decisions == 2 && state.actions == 1 && state.responses == 1);

        // Normal iframe requests go to the downstream delegate.
        request.targetFrame = [TestFrame new];
        request.request = [NSURLRequest requestWithURL:external];
        [adapter webView:(id)view decidePolicyForNavigationAction:(id)request
            decisionHandler:^(WKNavigationActionPolicy decision) {
                assert(decision == WKNavigationActionPolicyAllow);
                decisions++;
            }];
        assert(decisions == 3 && state.actions == 1);

        // A denied action, a popup, and a download each cancel once.
        request.targetFrame = top;
        void (^expectActionCancel)(WKNavigationActionPolicy) = ^(WKNavigationActionPolicy decision) {
            assert(decision == WKNavigationActionPolicyCancel);
            decisions++;
        };
        [adapter webView:(id)view decidePolicyForNavigationAction:(id)request decisionHandler:expectActionCancel];
        request.targetFrame = nil;
        request.request = top.request;
        [adapter webView:(id)view decidePolicyForNavigationAction:(id)request decisionHandler:expectActionCancel];
        request.targetFrame = top;
        request.shouldPerformDownload = YES;
        [adapter webView:(id)view decidePolicyForNavigationAction:(id)request decisionHandler:expectActionCancel];
        request.shouldPerformDownload = NO;
        assert(decisions == 6 && state.actions == 3);

        request.forMainFrame = NO;
        [adapter webView:(id)view decidePolicyForNavigationResponse:(id)request
            decisionHandler:^(WKNavigationResponsePolicy decision) {
                assert(decision == WKNavigationResponsePolicyAllow);
                decisions++;
            }];
        assert(decisions == 7 && state.responses == 1);

        WKNavigation *first = (id)[NSObject new];
        [adapter webView:(id)view didStartProvisionalNavigation:first];
        [adapter webView:(id)view didCommitNavigation:first];
        assert([adapter respondsToSelector:@selector(webView:didFinishNavigation:)]);
        [adapter webView:(id)view didFinishNavigation:first];
        assert(downstream.commits == 1 && downstream.finishes == 1);
        TestMessage *watch = [TestMessage new];
        watch.webView = view;
        watch.frameInfo = top;
        watch.body = @"watch";
        __block unsigned replies = 0;
        [view.configuration.handler userContentController:(id)view.configuration
            didReceiveScriptMessage:(id)watch replyHandler:^(id value, NSString *error) {
                assert([value isEqual:@"status"] && !error);
                replies++;
            }];
        uint64_t firstGeneration = state.generation;
        zlack_send_page_event((__bridge void *)view, firstGeneration, "status", true, NULL);
        assert(replies == 1);

        __block unsigned cancellations = 0;
        [view.configuration.handler userContentController:(id)view.configuration
            didReceiveScriptMessage:(id)watch replyHandler:^(id value, NSString *error) {
                assert(!value && error);
                cancellations++;
            }];
        WKNavigation *second = (id)[NSObject new];
        [adapter webView:(id)view didStartProvisionalNavigation:second];
        [adapter webView:(id)view didCommitNavigation:second];
        assert(cancellations == 1);
        [view.configuration.handler userContentController:(id)view.configuration
            didReceiveScriptMessage:(id)watch replyHandler:^(id value, NSString *error) {
                assert([value isEqual:@"current"] && !error);
                replies++;
            }];
        zlack_send_page_event((__bridge void *)view, firstGeneration, "stale", true, NULL);
        assert(replies == 1);
        zlack_send_page_event((__bridge void *)view, state.generation, "current", true, NULL);
        assert(replies == 2);

        TestFrame *iframe = [TestFrame new];
        iframe.request = top.request;
        watch.frameInfo = iframe;
        [view.configuration.handler userContentController:(id)view.configuration
            didReceiveScriptMessage:(id)watch replyHandler:^(id value, NSString *error) {
                assert(!value && error);
                cancellations++;
            }];
        assert(cancellations == 2);

        // A status consumes the live reply. The actions that arrive before the
        // page can watch again must retain order, including a contextless click.
        watch.frameInfo = top;
        TestContext *context = &state;
        void (^send)(NSString *, BOOL, const char *) = ^(NSString *value, BOOL isStatus, const char *fallback) {
            zlack_send_page_event((__bridge void *)view, context->generation, value.UTF8String, isStatus, fallback);
        };
        NSMutableArray<NSString *> *received = [NSMutableArray array];
        void (^observe)(void) = ^{
            [view.configuration.handler userContentController:(id)view.configuration
                didReceiveScriptMessage:(id)watch replyHandler:^(id value, NSString *error) {
                    assert(value && !error);
                    [received addObject:value];
                }];
        };
        observe();
        send(@"first status", YES, NULL);
        send(@"notification with no context", NO, NULL);
        send(@"second notification", NO, "https://app.slack.com/client/channel");
        send(@"navigate", NO, "https://app.slack.com/client/team");
        send(@"old status", YES, NULL);
        send(@"latest status", YES, NULL);
        assert(received.count == 1);
        for (unsigned n = 0; n < 4; n++) observe();
        assert([received isEqualToArray:(@[
            @"first status", @"notification with no context", @"second notification", @"navigate", @"latest status",
        ])]);

        [received removeAllObjects];
        for (unsigned n = 0; n < 17; n++) send([NSString stringWithFormat:@"%u", n], NO, NULL);
        for (unsigned n = 0; n < 16; n++) observe();
        assert(received.count == 16);
        for (unsigned n = 0; n < 16; n++) {
            assert(([received[n] isEqualToString:[NSString stringWithFormat:@"%u", n + 1]]));
        }

        [received removeAllObjects];
        send(@"discard old action", NO, NULL);
        send(@"discard old status", YES, NULL);
        WKNavigation *third = (id)[NSObject new];
        [adapter webView:(id)view didStartProvisionalNavigation:third];
        [adapter webView:(id)view didCommitNavigation:third];
        observe();
        assert(received.count == 0);
        send(@"new document status", YES, NULL);
        assert([received isEqualToArray:@[@"new document status"]]);

        TestView *signIn = [TestView new];
        signIn.navigationDelegate = downstream;
        signIn.configuration = [TestController new];
        assert(zlack_install_webview((__bridge void *)signIn, true, &state, policy, invoke, advance, dropContext));
        assert(signIn.configuration.handlersRemoved && signIn.configuration.scriptsRemoved);
        assert(signIn.configuration.handler == nil);

        // WebKit may have cached the adapter's selector list before teardown.
        downstream = nil;
        [adapter webView:(id)view didFinishNavigation:second];
    }
    return 0;
}
