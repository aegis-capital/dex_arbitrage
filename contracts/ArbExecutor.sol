// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// Minimal atomic two-leg arbitrage executor for Uniswap-V2-style pairs.
///
/// The contract holds an inventory of `baseToken` (e.g. WETH). `execute`
/// swaps base -> quote on `pairBuy`, then quote -> base on `pairSell`,
/// and reverts unless the contract's base-token balance grew by at least
/// `minProfit` — so a stale or front-run opportunity costs only gas.
///
/// Deployment (e.g. with Foundry):
///   forge create contracts/ArbExecutor.sol:ArbExecutor \
///     --constructor-args 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2 \
///     --private-key $PRIVATE_KEY --rpc-url $ETH_RPC_URL
/// then transfer WETH to the contract and set EXECUTOR_CONTRACT for the bot.
/// The deploying key is the owner and must match the bot's PRIVATE_KEY.

interface IUniswapV2Pair {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
}

interface IERC20 {
    function balanceOf(address owner) external view returns (uint256);
}

contract ArbExecutor {
    address public immutable owner;
    address public immutable baseToken;

    constructor(address _baseToken) {
        owner = msg.sender;
        baseToken = _baseToken;
    }

    modifier onlyOwner() {
        require(msg.sender == owner, "not owner");
        _;
    }

    function execute(
        address pairBuy,
        address pairSell,
        uint256 amountIn,
        uint256 minProfit
    ) external onlyOwner {
        uint256 balanceBefore = IERC20(baseToken).balanceOf(address(this));
        require(balanceBefore >= amountIn, "insufficient inventory");

        address quoteToken = _otherToken(pairBuy, baseToken);
        uint256 quoteOut = _swap(pairBuy, baseToken, amountIn);
        _swap(pairSell, quoteToken, quoteOut);

        uint256 balanceAfter = IERC20(baseToken).balanceOf(address(this));
        require(balanceAfter >= balanceBefore + minProfit, "unprofitable");
    }

    function withdraw(address token, uint256 amount) external onlyOwner {
        _safeTransfer(token, owner, amount);
    }

    /// Swaps `amountIn` of `tokenIn` on `pair`, output stays in this contract.
    function _swap(address pair, address tokenIn, uint256 amountIn) internal returns (uint256 amountOut) {
        (uint112 reserve0, uint112 reserve1, ) = IUniswapV2Pair(pair).getReserves();
        bool inIsToken0 = tokenIn == IUniswapV2Pair(pair).token0();
        (uint256 reserveIn, uint256 reserveOut) =
            inIsToken0 ? (uint256(reserve0), uint256(reserve1)) : (uint256(reserve1), uint256(reserve0));

        uint256 amountInWithFee = amountIn * 997;
        amountOut = (amountInWithFee * reserveOut) / (reserveIn * 1000 + amountInWithFee);

        _safeTransfer(tokenIn, pair, amountIn);
        (uint256 amount0Out, uint256 amount1Out) =
            inIsToken0 ? (uint256(0), amountOut) : (amountOut, uint256(0));
        IUniswapV2Pair(pair).swap(amount0Out, amount1Out, address(this), new bytes(0));
    }

    function _otherToken(address pair, address known) internal view returns (address) {
        address token0 = IUniswapV2Pair(pair).token0();
        return token0 == known ? IUniswapV2Pair(pair).token1() : token0;
    }

    /// Tolerates non-standard ERC20s (e.g. USDT) that return no boolean.
    function _safeTransfer(address token, address to, uint256 value) internal {
        (bool ok, bytes memory data) =
            token.call(abi.encodeWithSelector(0xa9059cbb, to, value));
        require(ok && (data.length == 0 || abi.decode(data, (bool))), "transfer failed");
    }
}
