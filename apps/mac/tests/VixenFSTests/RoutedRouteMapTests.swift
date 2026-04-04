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
    }

    func testItemPreservesExplicitBackendItemID() {
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
            backendItemId: 1
        )

        XCTAssertEqual(item.itemId, 1001)
        XCTAssertEqual(item.backendItemId, 1)
    }

    func testRootAttributesUseStableRootID() {
        let volume = Volume()
        let attrs = volume.rootAttributesForTesting()

        XCTAssertEqual(attrs.fileID.rawValue, 1)
        XCTAssertEqual(attrs.parentID.rawValue, 1)
        XCTAssertEqual(attrs.type, .directory)
    }
}
