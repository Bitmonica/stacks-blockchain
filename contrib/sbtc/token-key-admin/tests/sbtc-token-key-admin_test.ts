
import { Clarinet, Tx, Chain, Account, types } from 'https://deno.land/x/clarinet@v1.3.1/index.ts';
import { assertEquals } from 'https://deno.land/std@0.170.0/testing/asserts.ts';

Clarinet.test({
    name: "Ensure that coordinator starts empty",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let coordinator = chain.callReadOnlyFn("sbtc-token-key-admin", "get-coordinator-data", [], deployer.address);

        coordinator.result.expectNone();
    },
});

Clarinet.test({
    name: "Ensure that coordinator can be written then read",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-coordinator-data", [types.tuple({addr: types.principal(deployer.address), key: types.buff(0x000000000000000000000000000000000000000000000000000000000000000000)})], deployer.address),
        ]);

        let [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        let coordinator = chain.callReadOnlyFn("sbtc-token-key-admin", "get-coordinator-data", [], deployer.address);

        coordinator.result.expectSome({addr: deployer.address, key: 0x000000000000000000000000000000000000000000000000000000000000000000});
    },
});

Clarinet.test({
    name: "Ensure that num-keys can be written then read",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-num-keys", [types.uint(23)], deployer.address),
        ]);

        let [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        let coordinator = chain.callReadOnlyFn("sbtc-token-key-admin", "get-num-keys", [], deployer.address);

        coordinator.result.expectUint(23);
    },
});

Clarinet.test({
    name: "Ensure that num-parties can be written then read",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-num-parties", [types.uint(23)], deployer.address),
        ]);

        let [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        let coordinator = chain.callReadOnlyFn("sbtc-token-key-admin", "get-num-parties", [], deployer.address);

        coordinator.result.expectUint(23);
    },
});

Clarinet.test({
    name: "Ensure that threshold can be written then read",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-threshold", [types.uint(23)], deployer.address),
        ]);

        let [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        let coordinator = chain.callReadOnlyFn("sbtc-token-key-admin", "get-threshold", [], deployer.address);

        coordinator.result.expectUint(23);
    },
});

Clarinet.test({
    name: "Ensure that bitcoin-wallet-address can be written then read",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-bitcoin-wallet-address", [types.ascii("123456780abcdefghijklmnopqrstuvwxyz")], deployer.address),
        ]);

        let [receipt] = block.receipts;

        receipt.result.expectOk();

        let coordinator = chain.callReadOnlyFn("sbtc-token-key-admin", "get-bitcoin-wallet-address", [], deployer.address);

        coordinator.result.expectSome().expectAscii("123456780abcdefghijklmnopqrstuvwxyz");
    },
});

Clarinet.test({
    name: "Ensure that signer can be written then read",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-signer-data", [types.uint(1), types.tuple({addr: types.principal(deployer.address), key: types.buff(0x000000000000000000000000000000000000000000000000000000000000000000)})], deployer.address),
        ]);

        let [receipt] = block.receipts;

        receipt.result.expectOk();

        let coordinator = chain.callReadOnlyFn("sbtc-token-key-admin", "get-signer-data", [types.uint(1)], deployer.address);

        coordinator.result.expectSome(deployer.address);
    },
});

Clarinet.test({
    name: "Ensure that signer can be written then deleted",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let block_set = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-signer-data", [types.uint(1), types.tuple({addr: types.principal(deployer.address), key: types.buff(0x000000000000000000000000000000000000000000000000000000000000000000)})], deployer.address),            
        ]);

        let [receipt_set] = block_set.receipts;

        receipt_set.result.expectOk();

        let coordinator_set = chain.callReadOnlyFn("sbtc-token-key-admin", "get-signer-data", [types.uint(1)], deployer.address);

        coordinator_set.result.expectSome(deployer.address);

        let block_delete = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "delete-signer-data", [types.uint(1)], deployer.address),
        ]);

        let [receipt_delete] = block_delete.receipts;

        receipt_delete.result.expectOk();

        let coordinator_delete = chain.callReadOnlyFn("sbtc-token-key-admin", "get-signer-data", [types.uint(1)], deployer.address);

        coordinator_delete.result.expectNone();
    },
});

Clarinet.test({
    name: "Ensure that set-signer-info fails if the key-id is out of range",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-num-keys", [types.uint(23)], deployer.address),
        ]);

        let [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        let coordinator = chain.callReadOnlyFn("sbtc-token-key-admin", "get-num-keys", [], deployer.address);

        coordinator.result.expectUint(23);

        block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-signer-data", [types.uint(23), types.tuple({addr: types.principal(deployer.address), key: types.buff(0x000000000000000000000000000000000000000000000000000000000000000000)})], deployer.address),
        ]);

        [receipt] = block.receipts;

        receipt.result.expectErr();

    },
});

Clarinet.test({
    name: "Ensure we can mint tokens",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let balance = chain.callReadOnlyFn("sbtc-token-key-admin", "get-balance", [types.principal(deployer.address)], deployer.address);

        balance.result.expectOk().expectUint(0);

        let block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-coordinator-data", [types.tuple({addr: types.principal(deployer.address), key: types.buff(0x000000000000000000000000000000000000000000000000000000000000000000)})], deployer.address),
        ]);

        let [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "mint!", [types.uint(1234)], deployer.address),
        ]);

        [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        balance = chain.callReadOnlyFn("sbtc-token-key-admin", "get-balance", [types.principal(deployer.address)], deployer.address);

        balance.result.expectOk().expectUint(1234);
    },
});

Clarinet.test({
    name: "Ensure we can burn tokens",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let balance = chain.callReadOnlyFn("sbtc-token-key-admin", "get-balance", [types.principal(deployer.address)], deployer.address);

        balance.result.expectOk().expectUint(0);

        let block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-coordinator-data", [types.tuple({addr: types.principal(deployer.address), key: types.buff(0x000000000000000000000000000000000000000000000000000000000000000000)})], deployer.address),
        ]);

        let [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "mint!", [types.uint(1234)], deployer.address),
        ]);

        [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        balance = chain.callReadOnlyFn("sbtc-token-key-admin", "get-balance", [types.principal(deployer.address)], deployer.address);

        balance.result.expectOk().expectUint(1234);

        block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "burn!", [types.uint(4)], deployer.address),
        ]);

        [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        balance = chain.callReadOnlyFn("sbtc-token-key-admin", "get-balance", [types.principal(deployer.address)], deployer.address);

        balance.result.expectOk().expectUint(1230);
    },
});

Clarinet.test({
    name: "Ensure burning tokens fails if insufficient balance",
    async fn(chain: Chain, accounts: Map<string, Account>) {
        let deployer = accounts.get("deployer")!;

        let balance = chain.callReadOnlyFn("sbtc-token-key-admin", "get-balance", [types.principal(deployer.address)], deployer.address);

        balance.result.expectOk().expectUint(0);

        let block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "set-coordinator-data", [types.tuple({addr: types.principal(deployer.address), key: types.buff(0x000000000000000000000000000000000000000000000000000000000000000000)})], deployer.address),
        ]);

        let [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "mint!", [types.uint(1234)], deployer.address),
        ]);

        [receipt] = block.receipts;

        receipt.result.expectOk().expectBool(true);

        balance = chain.callReadOnlyFn("sbtc-token-key-admin", "get-balance", [types.principal(deployer.address)], deployer.address);

        balance.result.expectOk().expectUint(1234);

        block = chain.mineBlock([
            Tx.contractCall("sbtc-token-key-admin", "burn!", [types.uint(1235)], deployer.address),
        ]);

        [receipt] = block.receipts;

        receipt.result.expectErr();

        balance = chain.callReadOnlyFn("sbtc-token-key-admin", "get-balance", [types.principal(deployer.address)], deployer.address);

        balance.result.expectOk().expectUint(1234);
    },
});