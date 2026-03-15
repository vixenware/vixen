import FSKit
import XCTest
@testable import VixenFS

final class VixenFSTests: XCTestCase {
    func testSIDSegmentValidationAcceptsHexAndUUID() {
        XCTAssertTrue(Volume.isValidSidSegment("0123456789abcdef0123456789abcdef"))
        XCTAssertTrue(Volume.isValidSidSegment("01234567-89ab-cdef-0123-456789abcdef"))
    }

    func testSIDSegmentValidationRejectsNonSIDPathSegments() {
        XCTAssertFalse(Volume.isValidSidSegment(""))
        XCTAssertFalse(Volume.isValidSidSegment(".vixenfs"))
        XCTAssertFalse(Volume.isValidSidSegment("not-a-sid"))
        XCTAssertFalse(Volume.isValidSidSegment("0123456789abcdef0123456789abcdeg"))
    }

    func testItemDefaultsBackendItemIDToLocalItemID() {
        let attrs = FSItem.Attributes.fromVfs(
            itemId: 7,
            parentId: 1,
            itemType: .file,
            size: 42,
            modifiedTime: 10,
            createdTime: 5,
            mode: 0o644
        )

        let item = Item(itemId: 7, name: "file.txt", attributes: attrs)

        XCTAssertEqual(item.itemId, 7)
        XCTAssertEqual(item.backendItemId, 7)
        XCTAssertNil(item.sid)
        XCTAssertNil(item.prefix)
    }

    func testItemPreservesExplicitSIDAndBackendItemID() {
        let attrs = FSItem.Attributes.fromVfs(
            itemId: 1001,
            parentId: 1,
            itemType: .directory,
            size: 0,
            modifiedTime: 10,
            createdTime: 5,
            mode: 0o755
        )

        let item = Item(
            itemId: 1001,
            name: "root",
            attributes: attrs,
            prefix: nil,
            sid: "sid-1234",
            backendItemId: 1
        )

        XCTAssertEqual(item.itemId, 1001)
        XCTAssertEqual(item.backendItemId, 1)
        XCTAssertEqual(item.sid, "sid-1234")
    }
}
