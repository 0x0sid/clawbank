// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title AgentTreasury — ERC-4337 treasury with credit enforcement
/// @notice Validates UserOps against credit ceilings, time windows, and banker co-signatures.
/// @dev Deploy on Base (Sepolia testnet, then mainnet). USDC funded.

interface IEntryPoint {
    function getNonce(address sender, uint192 key) external view returns (uint256);
}

struct PackedUserOperation {
    address sender;
    uint256 nonce;
    bytes initCode;
    bytes callData;
    bytes32 accountGasLimits;
    uint256 preVerificationGas;
    bytes32 gasFees;
    bytes paymasterAndData;
    bytes signature;
}

contract AgentTreasury {
    address public banker;
    IEntryPoint private immutable _entryPoint;

    mapping(address => uint256) public creditCeiling;
    mapping(address => uint256) public creditSpent;
    mapping(address => uint256) public creditExpiry;

    uint256 internal constant SIG_VALIDATION_SUCCESS = 0;
    uint256 internal constant SIG_VALIDATION_FAILED = 1;

    event CreditGranted(address indexed agent, uint256 ceiling, uint256 expiry);
    event CreditRecalled(address indexed agent, string reason);

    modifier onlyBanker() {
        require(msg.sender == banker, "not banker");
        _;
    }

    constructor(address _banker, IEntryPoint entryPointAddr) {
        banker = _banker;
        _entryPoint = entryPointAddr;
    }

    /// @notice Grant a credit line to an agent.
    /// @param agent The agent's address.
    /// @param ceiling Maximum USD (in wei-scaled units) the agent can spend.
    /// @param expiry Unix timestamp after which the credit line is invalid.
    function grantCredit(address agent, uint256 ceiling, uint256 expiry)
        external
        onlyBanker
    {
        creditCeiling[agent] = ceiling;
        creditSpent[agent]   = 0;
        creditExpiry[agent]  = expiry;
        emit CreditGranted(agent, ceiling, expiry);
    }

    /// @notice Recall an agent's credit line immediately.
    /// @param agent The agent's address.
    /// @param reason Human-readable reason for the recall.
    function recallCredit(address agent, string calldata reason)
        external
        onlyBanker
    {
        creditCeiling[agent] = 0;
        emit CreditRecalled(agent, reason);
    }

    /// @notice Validate a UserOperation against credit constraints.
    /// @dev Called by the EntryPoint. Checks banker co-signature, expiry, and ceiling.
    function validateUserOp(
        PackedUserOperation calldata userOp,
        bytes32 userOpHash,
        uint256 /* missingAccountFunds */
    ) external returns (uint256) {
        // Decode dual signature: (agentSig, bankerSig)
        (, bytes memory bankerSig) = abi.decode(userOp.signature, (bytes, bytes));

        // Verify banker co-signed this operation
        if (_recoverSigner(userOpHash, bankerSig) != banker) {
            return SIG_VALIDATION_FAILED;
        }

        address agent  = userOp.sender;
        uint256 amount = _parseAmount(userOp.callData);

        // Time window check
        if (block.timestamp > creditExpiry[agent]) {
            return SIG_VALIDATION_FAILED;
        }

        // Cumulative spend check
        if (creditSpent[agent] + amount > creditCeiling[agent]) {
            return SIG_VALIDATION_FAILED;
        }

        // All checks passed — record spend
        creditSpent[agent] += amount;
        return SIG_VALIDATION_SUCCESS;
    }

    /// @notice Get the EntryPoint address.
    function entryPoint() public view returns (IEntryPoint) {
        return _entryPoint;
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// @dev Recover the signer from an ECDSA signature over a hash.
    function _recoverSigner(bytes32 hash, bytes memory sig)
        internal
        pure
        returns (address)
    {
        if (sig.length != 65) return address(0);

        bytes32 r;
        bytes32 s;
        uint8 v;

        assembly {
            r := mload(add(sig, 32))
            s := mload(add(sig, 64))
            v := byte(0, mload(add(sig, 96)))
        }

        if (v < 27) v += 27;

        return ecrecover(hash, v, r, s);
    }

    /// @dev Parse the transfer amount from callData.
    ///      Assumes standard ERC-20 transfer(address,uint256) encoding.
    function _parseAmount(bytes calldata callData)
        internal
        pure
        returns (uint256)
    {
        if (callData.length < 68) return 0;
        // Skip 4-byte selector + 32-byte address = offset 36, read 32 bytes
        return abi.decode(callData[36:68], (uint256));
    }
}
