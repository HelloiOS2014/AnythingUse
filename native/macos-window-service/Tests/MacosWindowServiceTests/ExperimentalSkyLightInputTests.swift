import CoreGraphics
import XCTest
@testable import MacosWindowService

final class ExperimentalSkyLightInputTests: XCTestCase {
    func testSyntheticFocusPlanContainsOnlyTargetIdentity() {
        let targetPSN = [UInt8](repeating: 7, count: 8)
        let targetWindowID = CGWindowID(0x1234_5678)
        let plan = skyLightSyntheticTargetFocusPlan(
            targetPSN: targetPSN,
            targetWindowID: targetWindowID
        )

        XCTAssertEqual(plan.activateTarget.psn, targetPSN)
        XCTAssertEqual(plan.deactivateTarget.psn, targetPSN)
        XCTAssertEqual(plan.activateTarget.windowID, targetWindowID)
        XCTAssertEqual(plan.deactivateTarget.windowID, targetWindowID)
        XCTAssertTrue(plan.activateTarget.focused)
        XCTAssertFalse(plan.deactivateTarget.focused)

        let focus = skyLightActivationRecord(windowID: targetWindowID, focused: true)
        let defocus = skyLightActivationRecord(windowID: targetWindowID, focused: false)
        XCTAssertEqual(focus.count, 0xF8)
        XCTAssertEqual(focus[0x8A], 0x01)
        XCTAssertEqual(defocus[0x8A], 0x02)
    }
}
