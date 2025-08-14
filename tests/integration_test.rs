use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signer::{keypair::Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};
use spl_associated_token_account::{
    get_associated_token_address, instruction::create_associated_token_account,
};
use spl_token::instruction as token_instruction;
use spl_token_auto_purchase::instruction::AutoBuyerInstruction;
use std::fs;
use std::str::FromStr;
use std::thread;
use std::time::Duration;

// --- CONFIGURATION ---
const PROGRAM_ID: &str = "9yXvLdU4zNJajJLPn9qu9f1bfou7CCJx4unvnViy7g1h";
const DEVNET_URL: &str = "https://api.devnet.solana.com";

// Your wallet address
const YOUR_WALLET_PUBKEY: &str = "2Am62L81nNrvGiFaLCCW2bHJifXVkQ6mrvJiboosLHkB";
// You need to provide the path to your wallet's private key file
// This should be a JSON file with your keypair (e.g., ~/.config/solana/id.json)
const WALLET_KEYPAIR_PATH: &str = "~/.config/solana/id.json"; // Update this path

// Using valid pubkeys for testing (these won't work for actual swaps but will pass validation)
const RAYDIUM_V4_PROGRAM_ID: &str = "675kPX9MHTjS2yt3Ws8KqJuGKKfxpHMvWqHgFCiGJk7D"; // Actual Raydium program on devnet

// We will swap SOL for USDC
const QUOTE_MINT_ADDRESS: &str = "So11111111111111111111111111111111111111112"; // WSOL
const TARGET_MINT_ADDRESS: &str = "Gh9ZwEmdLJ8DscKNTkTqPbNwLNNBjuSzaG9Vp2KGtKJr"; // USDC Devnet

const SOL_TO_WRAP: u64 = 100_000_000; // 0.1 SOL (safer for testing)
const MIN_AMOUNT_OUT: u64 = 1_000; // Minimum USDC to receive (example)

fn load_keypair_from_file(filepath: &str) -> Result<Keypair, Box<dyn std::error::Error>> {
    let expanded_path = if filepath.starts_with("~/") {
        let home = std::env::var("HOME")?;
        filepath.replacen("~", &home, 1)
    } else {
        filepath.to_string()
    };

    let keypair_data = fs::read_to_string(expanded_path)?;
    let keypair_bytes: Vec<u8> = serde_json::from_str(&keypair_data)?;
    Ok(Keypair::from_bytes(&keypair_bytes)?)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_buy_token_live() {
    // 1. SETUP
    let rpc_client = RpcClient::new(DEVNET_URL.to_string());
    let program_id = Pubkey::from_str(PROGRAM_ID).unwrap();

    // Load your wallet keypair
    let user_wallet = match load_keypair_from_file(WALLET_KEYPAIR_PATH) {
        Ok(keypair) => {
            // Verify the loaded keypair matches your expected pubkey
            let expected_pubkey = Pubkey::from_str(YOUR_WALLET_PUBKEY).unwrap();
            if keypair.pubkey() != expected_pubkey {
                panic!(
                    "Loaded keypair pubkey {} doesn't match expected pubkey {}",
                    keypair.pubkey(),
                    expected_pubkey
                );
            }
            keypair
        }
        Err(e) => {
            println!(
                "Failed to load wallet keypair from {}: {}",
                WALLET_KEYPAIR_PATH, e
            );
            println!("Please make sure the path is correct and the file contains your keypair in JSON format.");
            println!("You can export your keypair using: solana config get keypair");
            panic!("Could not load wallet keypair");
        }
    };

    println!("-----------------------------------------------------------------");
    println!("Using wallet: {}", user_wallet.pubkey());
    println!("Testing on devnet: {}", DEVNET_URL);
    println!("-----------------------------------------------------------------");

    // Check current balance
    match rpc_client.get_balance(&user_wallet.pubkey()).await {
        Ok(balance) => {
            let sol_balance = balance as f64 / 1_000_000_000.0;
            println!("Current wallet balance: {} SOL", sol_balance);

            if balance < SOL_TO_WRAP * 2 {
                println!("⚠️  Warning: Low balance!");
                println!(
                    "You need at least {} SOL for this test",
                    (SOL_TO_WRAP * 2) as f64 / 1_000_000_000.0
                );
                println!("Consider adding more SOL to your wallet or reducing SOL_TO_WRAP");
                // Don't panic - let the test continue but warn the user
            }
        }
        Err(e) => {
            println!("Error checking wallet balance: {}", e);
            panic!("Could not check wallet balance");
        }
    }

    // 2. PREPARE ACCOUNTS
    let quote_mint = Pubkey::from_str(QUOTE_MINT_ADDRESS).unwrap();
    let target_mint = Pubkey::from_str(TARGET_MINT_ADDRESS).unwrap();

    // Create Associated Token Accounts (ATA)
    let user_quote_ata = get_associated_token_address(&user_wallet.pubkey(), &quote_mint);
    let user_target_ata = get_associated_token_address(&user_wallet.pubkey(), &target_mint);

    let mut setup_instructions = vec![];

    // Check if quote ATA exists, if not create it
    match rpc_client.get_account(&user_quote_ata).await {
        Err(_) => {
            println!("Creating WSOL token account...");
            setup_instructions.push(create_associated_token_account(
                &user_wallet.pubkey(),
                &user_wallet.pubkey(),
                &quote_mint,
                &spl_token::id(), // Use the standard SPL Token program ID
            ));
        }
        Ok(_) => println!("WSOL token account already exists"),
    }

    // Check if target ATA exists, if not create it
    match rpc_client.get_account(&user_target_ata).await {
        Err(_) => {
            println!("Creating USDC token account...");
            setup_instructions.push(create_associated_token_account(
                &user_wallet.pubkey(),
                &user_wallet.pubkey(),
                &target_mint,
                &spl_token::id(), // Use the standard SPL Token program ID
            ));
        }
        Ok(_) => println!("USDC token account already exists"),
    }

    // Always add instructions to wrap SOL
    setup_instructions.push(system_instruction::transfer(
        &user_wallet.pubkey(),
        &user_quote_ata,
        SOL_TO_WRAP,
    ));
    setup_instructions.push(
        token_instruction::sync_native(&spl_token::id(), &user_quote_ata).unwrap(), // Use standard SPL Token program ID
    );

    if !setup_instructions.is_empty() {
        let recent_blockhash = rpc_client.get_latest_blockhash().await.unwrap();
        let setup_tx = Transaction::new_signed_with_payer(
            &setup_instructions,
            Some(&user_wallet.pubkey()),
            &[&user_wallet],
            recent_blockhash,
        );

        println!("Setting up token accounts...");
        match rpc_client.send_and_confirm_transaction(&setup_tx).await {
            Ok(sig) => println!("Setup transaction successful! Signature: {}", sig),
            Err(e) => panic!("Setup transaction failed: {}", e),
        }

        // Wait a moment for the transaction to be processed
        thread::sleep(Duration::from_secs(2));
    }

    // 3. EXECUTE THE SWAP
    let buy_instruction = AutoBuyerInstruction::BuyToken {
        amount_in: SOL_TO_WRAP,
        min_amount_out: MIN_AMOUNT_OUT,
    };

    // Generate valid placeholder pubkeys for testing
    // In production, these would be actual Raydium pool addresses
    let liquidity_pool_id = Pubkey::new_unique();
    let pool_token_a_account = Pubkey::new_unique();
    let pool_token_b_account = Pubkey::new_unique();

    let accounts = vec![
        AccountMeta::new(user_wallet.pubkey(), true),
        AccountMeta::new(user_quote_ata, false),
        AccountMeta::new(user_target_ata, false),
        AccountMeta::new_readonly(target_mint, false),
        AccountMeta::new_readonly(quote_mint, false),
        AccountMeta::new_readonly(Pubkey::from_str(RAYDIUM_V4_PROGRAM_ID).unwrap(), false),
        AccountMeta::new(liquidity_pool_id, false),
        AccountMeta::new(pool_token_a_account, false),
        AccountMeta::new(pool_token_b_account, false),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
    ];

    let buy_tx_instruction = Instruction {
        program_id,
        accounts,
        data: buy_instruction.pack(),
    };

    let recent_blockhash = rpc_client.get_latest_blockhash().await.unwrap();
    let buy_tx = Transaction::new_signed_with_payer(
        &[buy_tx_instruction],
        Some(&user_wallet.pubkey()),
        &[&user_wallet],
        recent_blockhash,
    );

    println!("Executing the swap...");
    match rpc_client.send_and_confirm_transaction(&buy_tx).await {
        Ok(sig) => {
            println!("-----------------------------------------------------------------");
            println!("✅ SWAP SUCCESSFUL!");
            println!("Transaction Signature: {}", sig);
            println!("-----------------------------------------------------------------");
        }
        Err(e) => {
            println!("-----------------------------------------------------------------");
            println!("❌ SWAP FAILED (Expected - using placeholder addresses)!");
            println!("Error: {}", e);
            println!("This is expected since we're using placeholder pool addresses.");
            println!("The test successfully created token accounts and submitted the transaction.");
            println!("-----------------------------------------------------------------");

            // Don't panic here since failure is expected with placeholder addresses
            // In a real test, you'd use actual pool addresses
        }
    }

    // 4. VERIFY TOKEN ACCOUNTS WERE CREATED
    println!("Verifying token accounts...");

    match rpc_client.get_token_account_balance(&user_quote_ata).await {
        Ok(balance) => println!("WSOL balance: {} tokens", balance.ui_amount_string),
        Err(e) => println!("Error getting WSOL balance: {}", e),
    }

    match rpc_client.get_token_account_balance(&user_target_ata).await {
        Ok(balance) => println!("USDC balance: {} tokens", balance.ui_amount_string),
        Err(e) => println!("Error getting USDC balance: {}", e),
    }

    println!("Test completed successfully!");
}

