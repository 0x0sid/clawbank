// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../AgentTreasury.sol";

/// @title AgentTreasury test suite
/// @dev Run with: forge test -vvv
contract AgentTreasuryTest is Test {
    AgentTreasury treasury;
    address banker = address(0xBABE);
    address entryPoint = address(0xE17);
    address agent1 = address(0xA1);
    address agent2 = address(0xA2);

    function setUp() public {
        vm.prank(banker);
        treasury = new AgentTreasury(banker, IEntryPoint(entryPoint));
    }

    // -----------------------------------------------------------------------
    // grantCredit
    // -----------------------------------------------------------------------

    function test_grantCredit_sets_fields() public {
        vm.prank(banker);
        treasury.grantCredit(agent1, 10_000e6, block.timestamp + 1 days);

        assertEq(treasury.creditCeiling(agent1), 10_000e6);
        assertEq(treasury.creditSpent(agent1), 0);
        assertEq(treasury.creditExpiry(agent1), block.timestamp + 1 days);
    }

    function test_grantCredit_resets_spent() public {
        // Simulate some prior spend by granting, then re-granting
        vm.startPrank(banker);
        treasury.grantCredit(agent1, 10_000e6, block.timestamp + 1 days);
        // Re-grant should reset spent to 0
        treasury.grantCredit(agent1, 20_000e6, block.timestamp + 2 days);
        vm.stopPrank();

        assertEq(treasury.creditSpent(agent1), 0);
        assertEq(treasury.creditCeiling(agent1), 20_000e6);
    }

    function test_grantCredit_only_banker() public {
        vm.prank(agent1);
        vm.expectRevert("not banker");
        treasury.grantCredit(agent1, 10_000e6, block.timestamp + 1 days);
    }

    function test_grantCredit_emits_event() public {
        vm.prank(banker);
        vm.expectEmit(true, false, false, true);
        emit AgentTreasury.CreditGranted(agent1, 10_000e6, block.timestamp + 1 days);
        treasury.grantCredit(agent1, 10_000e6, block.timestamp + 1 days);
    }

    // -----------------------------------------------------------------------
    // recallCredit
    // -----------------------------------------------------------------------

    function test_recallCredit_zeros_ceiling() public {
        vm.startPrank(banker);
        treasury.grantCredit(agent1, 10_000e6, block.timestamp + 1 days);
        treasury.recallCredit(agent1, "max loss exceeded");
        vm.stopPrank();

        assertEq(treasury.creditCeiling(agent1), 0);
    }

    function test_recallCredit_only_banker() public {
        vm.prank(agent1);
        vm.expectRevert("not banker");
        treasury.recallCredit(agent1, "unauthorized");
    }

    function test_recallCredit_emits_event() public {
        vm.startPrank(banker);
        treasury.grantCredit(agent1, 10_000e6, block.timestamp + 1 days);

        vm.expectEmit(true, false, false, true);
        emit AgentTreasury.CreditRecalled(agent1, "test recall");
        treasury.recallCredit(agent1, "test recall");
        vm.stopPrank();
    }

    // -----------------------------------------------------------------------
    // validateUserOp — access control
    // -----------------------------------------------------------------------

    function test_validateUserOp_only_entrypoint() public {
        vm.prank(agent1);
        PackedUserOperation memory op;
        vm.expectRevert("not entrypoint");
        treasury.validateUserOp(op, bytes32(0), 0);
    }

    // -----------------------------------------------------------------------
    // withdrawToken — access control
    // -----------------------------------------------------------------------

    function test_withdrawToken_only_banker() public {
        vm.prank(agent1);
        vm.expectRevert("not banker");
        treasury.withdrawToken(address(0xUSDC), agent1, 100e6);
    }

    // -----------------------------------------------------------------------
    // receive
    // -----------------------------------------------------------------------

    function test_receive_eth() public {
        vm.deal(banker, 1 ether);
        vm.prank(banker);
        (bool ok,) = address(treasury).call{value: 0.5 ether}("");
        assertTrue(ok);
        assertEq(address(treasury).balance, 0.5 ether);
    }

    // -----------------------------------------------------------------------
    // Fuzz: grantCredit ceiling and expiry
    // -----------------------------------------------------------------------

    function testFuzz_grantCredit(uint256 ceiling, uint256 expiry) public {
        vm.prank(banker);
        treasury.grantCredit(agent1, ceiling, expiry);
        assertEq(treasury.creditCeiling(agent1), ceiling);
        assertEq(treasury.creditExpiry(agent1), expiry);
    }
}
