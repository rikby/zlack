(() => {
    if (window !== window.top || window.location.origin !== 'https://app.slack.com') return false;
    return /^\/(?:client|archives)(?:\/|$)/.test(window.location.pathname);
})()
