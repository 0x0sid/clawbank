// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "../src/AgentTreasury.sol";

contract DeployScript is Script {
    // ERC-4337 EntryPoint v0.6 canonical address (same on all chains)
    address constant ENTRY_POINT = 0x5FF137D4b0FDCD49DcA30c7CF57E578a026d2789;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("BANKER_KEY");
        address banker = vm.addr(deployerPrivateKey);

        vm.startBroadcast(deployerPrivateKey);
        AgentTreasury treasury = new AgentTreasury(banker, IEntryPoint(ENTRY_POINT));
        vm.stopBroadcast();

        console.log("AgentTreasury deployed at:", address(treasury));
        console.log("Banker address:", banker);
    }
}
