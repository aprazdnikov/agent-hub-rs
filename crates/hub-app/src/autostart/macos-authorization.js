function run(argv) {
    ObjC.import('Foundation');
    ObjC.import('ServiceManagement');
    var service = $.NSClassFromString('SMAppService');
    if (service.isNil()) {
        return 'legacy';
    }
    var status = service.statusForLegacyURL($.NSURL.fileURLWithPath(argv[0]));
    // JXA may return this NSInteger enum as a string, and does not expose its
    // constants. Use SMAppServiceStatus values from Apple's SMAppService.h.
    // Match exact values: coercion would turn null/false/empty input into zero.
    switch (status) {
        case 0: case '0': return 'not-registered';
        case 1: case '1': return 'enabled';
        case 2: case '2': return 'requires-approval';
        case 3: case '3': return 'not-found';
    }
    throw new Error('Unknown Service Management authorization status');
}
