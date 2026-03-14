import FSKit
import XCTest
@testable import VixenFS

final class VixenFSTests: XCTestCase {
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
