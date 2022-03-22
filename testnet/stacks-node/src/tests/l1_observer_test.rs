use std;
use std::process::{Child, Command, Stdio};
use std::thread;

use crate::Config;

use stacks::burnchains::Burnchain;
use crate::neon;
use crate::ConfigFile;
use std::env;
use std::io::{BufRead, BufReader};
use std::time::Duration;


#[derive(std::fmt::Debug)]
pub enum SubprocessError {
    SpawnFailed(String),
}

type SubprocessResult<T> = Result<T, SubprocessError>;

/// In charge of running L1 `stacks-node`.
pub struct StacksMainchainController {
    sub_process: Option<Child>,
    config: Config,
    out_reader: Option<BufReader<std::process::ChildStdout>>,
}

impl StacksMainchainController {
    pub fn new(config: Config) -> StacksMainchainController {
        StacksMainchainController {
            sub_process: None,
            config,
            out_reader: None,
        }
    }

    pub fn start_process(&mut self) -> SubprocessResult<()> {
        let base_dir = env::var("STACKS_BASE_DIR").expect("couldn't read STACKS_BASE_DIR");
        let bin_file = format!("{}/target/release/stacks-node", &base_dir);
        let toml_file = format!(
            "{}/testnet/stacks-node/conf/mocknet-miner-conf.toml",
            &base_dir
        );
        let toml_content = ConfigFile::from_path(&toml_file);
        let mut command = Command::new(&bin_file);
        command
            .stdout(Stdio::piped())
            .arg("start")
            .arg("--config=".to_owned() + &toml_file);

        info!("stacks-node mainchain spawn: {:?}", command);

        let mut process = match command.spawn() {
            Ok(child) => child,
            Err(e) => return Err(SubprocessError::SpawnFailed(format!("{:?}", e))),
        };

        info!("stacks-node mainchain spawned, waiting for startup");
        let mut out_reader = BufReader::new(process.stdout.take().unwrap());

        let mut line = String::new();
        while let Ok(bytes_read) = out_reader.read_line(&mut line) {
            if bytes_read == 0 {
                return Err(SubprocessError::SpawnFailed(
                    "Stacks L1 closed before spawning network".into(),
                ));
            }
            info!("{:?}", &line);
            break;
        }

        info!("Stacks L1 startup finished");

        self.sub_process = Some(process);
        self.out_reader = Some(out_reader);

        Ok(())
    }

    pub fn kill_process(&mut self) {
        if let Some(mut sub_process) = self.sub_process.take() {
            sub_process.kill().unwrap();
        }
    }
}

impl Drop for StacksMainchainController {
    fn drop(&mut self) {
        self.kill_process();
    }
}

/// This test brings up bitcoind, and Stacks-L1, and ensures that our listener can hear and record burn blocks
/// from the Stacks-L1 chain.
#[test]
fn l1_observer_test() {
    let base_dir = env::current_dir().expect("couldn't read the path");
    info!("path: {:?}", &base_dir);
    let l1_toml_file = format!(
        "../../contrib/conf/stacks-l1-mocknet.toml", 
    );
    info!("l1_toml_file {:?}", &l1_toml_file);
    let toml_content = ConfigFile::from_path(&l1_toml_file);
    let conf = Config::from_config_file(toml_content);

    // Start Stacks L1.
    let mut stacks_controller = StacksMainchainController::new(conf.clone());
    let _stacks_res = stacks_controller
        .start_process()
        .expect("stacks l1 controller didn't start");

    // Start a run loop.
    let config = super::new_test_conf();
    let mut run_loop = neon::RunLoop::new(config.clone());
    let channel = run_loop.get_coordinator_channel().unwrap();
    thread::spawn(move || run_loop.start(None, 0));

    // Sleep to give the
    thread::sleep(Duration::from_millis(30000));

    // The burnchain should have registered what the listener recorded.
    let burnchain = Burnchain::new(&config.get_burn_db_path(), "mockstack", "hyperchain").unwrap();
    let (_, burndb) = burnchain.open_db(true).unwrap();
    let tip = burndb
        .get_canonical_chain_tip()
        .expect("couldn't get chain tip");
    info!("burnblock chain tip is {:?}", &tip);

    // Ensure that the tip height has moved beyond height 0.
    // We check that we have moved past 3 just to establish we are reliably getting blocks.
    assert!(tip.block_height > 3);

    channel.stop_chains_coordinator();
    stacks_controller.kill_process();
}
