import XCTest
@testable import VixenFS

final class RoutedRouteMapTests: XCTestCase {
    func testMultiSessionUnionVisibility() {
        let routes = [
            RoutedPrefix(prefix: "alpha", sid: "sid-a"),
            RoutedPrefix(prefix: "beta", sid: "sid-b"),
        ]

        let map = RoutedRouteMap.build(from: routes)

        XCTAssertEqual(map["alpha"], "sid-a")
        XCTAssertEqual(map["beta"], "sid-b")
        XCTAssertEqual(map.count, 2)
    }

    func testOneSessionRemovalIsolation() {
        let base = [
            "alpha": "sid-a",
            "beta": "sid-b",
            "gamma": "sid-a",
        ]

        let filtered = RoutedRouteMap.removingSid("sid-a", from: base)

        XCTAssertEqual(filtered["beta"], "sid-b")
        XCTAssertNil(filtered["alpha"])
        XCTAssertNil(filtered["gamma"])
        XCTAssertEqual(filtered.count, 1)
    }

    func testDeadSessionPurgeRemovesOnlyThatSid() {
        let routes = [
            RoutedPrefix(prefix: "alpha", sid: "sid-dead"),
            RoutedPrefix(prefix: "beta", sid: "sid-live"),
            RoutedPrefix(prefix: "gamma", sid: "sid-live"),
        ]
        let map = RoutedRouteMap.build(from: routes)

        let purged = RoutedRouteMap.removingSid("sid-dead", from: map)

        XCTAssertNil(purged["alpha"])
        XCTAssertEqual(purged["beta"], "sid-live")
        XCTAssertEqual(purged["gamma"], "sid-live")
        XCTAssertEqual(purged.count, 2)
    }
}
