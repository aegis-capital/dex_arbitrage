// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// Minimal atomic multi-leg arbitrage executor for constant-product pools
/// with the UniswapV2 pair interface (Uniswap V2, SushiSwap, Aerodrome/Solidly
/// volatile pools, ...).
///
/// The contract holds an inventory of `baseToken` (e.g. WETH). `execute`
/// swaps along `path` through `pairs` — the path must start and end at
/// `baseToken` — and reverts unless the contract's base-token balance grew by
/// at least `minProfit`, so a stale or front-run opportunity costs only gas.
///
/// Deployment (e.g. with Foundry):
///   forge create contracts/ArbExecutor.sol:ArbExecutor \
///     --constructor-args <WETH address for the target chain> \
///     --private-key $PRIVATE_KEY --rpc-url $ETH_RPC_URL
/// then transfer WETH to the contract and set EXECUTOR_CONTRACT for the bot.
/// The deploying key is the owner and must match the bot's PRIVATE_KEY.

interface IUniswapV2Pair {
    function token0() external view returns (address);
    // Declared as uint256 words so both Uniswap V2 pairs (uint112) and
    // Solidly pools (uint256) decode correctly.
    function getReserves() external view returns (uint256 reserve0, uint256 reserve1, uint256 blockTimestampLast);
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
}

interface IUniswapV3Pool {
    function token0() external view returns (address);
    function swap(
        address recipient,
        bool zeroForOne,
        int256 amountSpecified,
        uint160 sqrtPriceLimitX96,
        bytes calldata data
    ) external returns (int256 amount0, int256 amount1);
}

interface IERC20 {
    function balanceOf(address owner) external view returns (uint256);
}

contract ArbExecutor {
    // TickMath bounds; passing limit±1 means "no price limit" for exact-input swaps.
    uint160 private constant MIN_SQRT_RATIO = 4295128739;
    uint160 private constant MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342;

    address public immutable owner;
    address public immutable baseToken;

    /// Pool allowed to invoke uniswapV3SwapCallback, set only for the
    /// duration of a V3 swap.
    address private expectedV3Pool;

    constructor(address _baseToken) {
        owner = msg.sender;
        baseToken = _baseToken;
    }

    modifier onlyOwner() {
        require(msg.sender == owner, "not owner");
        _;
    }

    /// path[i] is the input token of pairs[i]; path has one more entry than
    /// pairs and must begin and end with baseToken. feesBps[i] is the pool's
    /// fee in 1/10000 units (30 = 0.3%), used only for V2-style legs.
    /// poolKinds[i]: 0 = V2-style pair, 1 = Uniswap V3 pool.
    function execute(
        address[] calldata pairs,
        address[] calldata path,
        uint256[] calldata feesBps,
        uint8[] calldata poolKinds,
        uint256 amountIn,
        uint256 minProfit
    ) external onlyOwner {
        uint256 n = pairs.length;
        require(
            n >= 2 && path.length == n + 1 && feesBps.length == n && poolKinds.length == n,
            "bad path shape"
        );
        require(path[0] == baseToken && path[n] == baseToken, "path must cycle baseToken");

        uint256 balanceBefore = IERC20(baseToken).balanceOf(address(this));
        require(balanceBefore >= amountIn, "insufficient inventory");

        uint256 amount = amountIn;
        for (uint256 i = 0; i < n; i++) {
            amount = poolKinds[i] == 0
                ? _swap(pairs[i], path[i], amount, feesBps[i])
                : _swapV3(pairs[i], path[i], amount);
        }

        uint256 balanceAfter = IERC20(baseToken).balanceOf(address(this));
        require(balanceAfter >= balanceBefore + minProfit, "unprofitable");
    }

    /// Exact-input V3 swap; the pool collects payment via the callback below.
    function _swapV3(address pool, address tokenIn, uint256 amountIn)
        internal
        returns (uint256 amountOut)
    {
        require(amountIn <= uint256(type(int256).max), "amount too large");
        bool zeroForOne = tokenIn == IUniswapV3Pool(pool).token0();
        expectedV3Pool = pool;
        (int256 amount0, int256 amount1) = IUniswapV3Pool(pool).swap(
            address(this),
            zeroForOne,
            int256(amountIn),
            zeroForOne ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1,
            abi.encode(tokenIn)
        );
        expectedV3Pool = address(0);
        amountOut = uint256(-(zeroForOne ? amount1 : amount0));
    }

    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external {
        require(msg.sender == expectedV3Pool, "unexpected callback");
        address tokenIn = abi.decode(data, (address));
        uint256 owed = uint256(amount0Delta > 0 ? amount0Delta : amount1Delta);
        _safeTransfer(tokenIn, msg.sender, owed);
    }

    function withdraw(address token, uint256 amount) external onlyOwner {
        _safeTransfer(token, owner, amount);
    }

    /// Swaps `amountIn` of `tokenIn` on `pair`, output stays in this contract.
    function _swap(address pair, address tokenIn, uint256 amountIn, uint256 feeBps)
        internal
        returns (uint256 amountOut)
    {
        (uint256 reserve0, uint256 reserve1, ) = IUniswapV2Pair(pair).getReserves();
        bool inIsToken0 = tokenIn == IUniswapV2Pair(pair).token0();
        (uint256 reserveIn, uint256 reserveOut) =
            inIsToken0 ? (reserve0, reserve1) : (reserve1, reserve0);

        uint256 amountInWithFee = amountIn * (10000 - feeBps);
        amountOut = (amountInWithFee * reserveOut) / (reserveIn * 10000 + amountInWithFee);

        _safeTransfer(tokenIn, pair, amountIn);
        (uint256 amount0Out, uint256 amount1Out) =
            inIsToken0 ? (uint256(0), amountOut) : (amountOut, uint256(0));
        IUniswapV2Pair(pair).swap(amount0Out, amount1Out, address(this), new bytes(0));
    }

    /// Tolerates non-standard ERC20s (e.g. USDT) that return no boolean.
    function _safeTransfer(address token, address to, uint256 value) internal {
        (bool ok, bytes memory data) =
            token.call(abi.encodeWithSelector(0xa9059cbb, to, value));
        require(ok && (data.length == 0 || abi.decode(data, (bool))), "transfer failed");
    }
}
