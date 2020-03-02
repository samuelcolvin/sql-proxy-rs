extern crate mariadb_proxy;

#[macro_use]
extern crate log;

use abci::*;
use env_logger;
use futures_util::future::FutureExt;
use hyper::{
    body::Body,
    client::{Client, HttpConnector},
};
use mysql::{Pool};
// use mysql_async;
use mariadb_proxy::{
    packet::{Packet, PacketType},
    packet_handler::PacketHandler,
};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use sqlparser::{dialect::GenericDialect, parser::Parser};
use tokio;

const DELIMITER: &str = "!_!";

// Convert incoming tx data to Sql string
fn convert_tx(tx: &[u8]) -> String {
    String::from_utf8(tx.to_vec()).unwrap()
}

fn run_query_sync(sql: String) {
    let database_url = "mysql://root:devpassword@mariadb:3306/mariadb";
    let pool = Pool::new(database_url).unwrap();

    info!("run_query_sync(): {}", sql);

    if sql.len() > 0 {
        let result = pool.prep_exec(sql, ());

        match result {
            Ok(_) => {
                info!("Query successfully executed");
            }
            Err(_e) => {
                info!("Query error: {}", _e);
            }
        }
    }
}

struct AbciApp {
    node_id: String,
    sql: String,
}

impl AbciApp {
    fn new(node_id: String) -> AbciApp {
        AbciApp {
            node_id: node_id,
            sql: String::from(""),
        }
    }
}

impl Application for AbciApp {
    /// Query Connection: Called on startup from Tendermint.  The application should normally
    /// return the last know state so Tendermint can determine if it needs to replay blocks
    /// to the application.
    fn info(&mut self, _req: &RequestInfo) -> ResponseInfo {
        info!("info()");
        ResponseInfo::new()
    }

    /// Query Connection: Set options on the application (rarely used)
    fn set_option(&mut self, _req: &RequestSetOption) -> ResponseSetOption {
        info!("set_option()");
        ResponseSetOption::new()
    }

    /// Query Connection: Query your application. This usually resolves through a merkle tree holding
    /// the state of the app.
    fn query(&mut self, _req: &RequestQuery) -> ResponseQuery {
        info!("query()");
        ResponseQuery::new()
    }

    /// Consensus Connection:  Called once on startup. Usually used to establish initial (genesis)
    /// state.
    fn init_chain(&mut self, _req: &RequestInitChain) -> ResponseInitChain {
        info!("init_chain()");
        ResponseInitChain::new()
    }

    /// Consensus Connection: Called at the start of processing a block of transactions
    /// The flow is:
    /// begin_block()
    ///   deliver_tx()  for each transaction in the block
    /// end_block()
    /// commit()
    fn begin_block(&mut self, _req: &RequestBeginBlock) -> ResponseBeginBlock {
        info!("begin_block()");
        ResponseBeginBlock::new()
    }

    /// Consensus Connection: Called at the end of the block.  Often used to update the validator set.
    fn end_block(&mut self, _req: &RequestEndBlock) -> ResponseEndBlock {
        info!("end_block()");
        ResponseEndBlock::new()
    }

    // Validate transactions.  Rule: SQL string must be valid SQL
    fn check_tx(&mut self, req: &RequestCheckTx) -> ResponseCheckTx {
        info!("check_tx()");

        let sql = convert_tx(req.get_tx());
        info!("Sql query: {}", sql);

        let dialect = GenericDialect {};
        let mut resp = ResponseCheckTx::new();
        // TODO: cover sql injection
        match Parser::parse_sql(&dialect, sql.clone()) {
            Ok(_val) => {
                info!("Valid SQL");
                // Update state to keep state correct for next check_tx call
                self.sql = sql;
            }
            Err(_e) => {
                info!("Invalid SQL");
                // Return error
                resp.set_code(1);
                resp.set_log(String::from("Must be valid sql!"));
            }
        }
        return resp;
    }

    fn deliver_tx(&mut self, req: &RequestDeliverTx) -> ResponseDeliverTx {
        info!("deliver_tx()");

        // Get the Tx [u8]
        let sql = convert_tx(req.get_tx());
        // Update state
        self.sql = sql;
        // Return default code 0 == bueno
        ResponseDeliverTx::new()
    }

    fn commit(&mut self, _req: &RequestCommit) -> ResponseCommit {
        info!("commit() {}", self.sql);

        // Run Query
        //Runtime::new().unwrap().block_on(run_query_async(&self.sql));
        run_query_sync(self.sql.clone());

        // Create the response
        let mut resp = ResponseCommit::new();

        // Set data so last state is included in the block
        let bytes = self.sql.as_bytes();
        resp.set_data(bytes.to_vec());

        self.sql = String::from("");
        resp
    }
}

struct ProxyHandler {
    node_id: String,
    http_client: Client<HttpConnector, Body>,
}

// Just forward the packet
impl PacketHandler for ProxyHandler {
    fn handle_request(&mut self, p: &Packet) -> Packet {
        // Print out the packet
        //debug!("[{}]", String::from_utf8_lossy(&p.bytes));

        if let Ok(PacketType::ComQuery) = p.packet_type() {
            let payload = &p.bytes[5..];
            let sql = String::from_utf8(payload.to_vec()).expect("Invalid UTF-8");
            info!("SQL: {}", sql);
            let mut url: String = "http://localhost:26657/broadcast_tx_commit?tx=".to_owned();
            url.push_str(&self.node_id);
            url.push_str(DELIMITER);
            url.push_str(&sql);
            info!("Pushing to Tendermint: {}", url);
            let _fut = self.http_client.get(url.parse().unwrap()).then(|res| {
                async move {
                    let response = res.unwrap();
                    debug!("Response: {}", response.status());
                    debug!("Headers: {:#?}\n", response.headers());
                }
            });
        } else {
            debug!("{:?} packet", p.packet_type());
        }

        p.clone()
    }

    fn handle_response(&mut self, p: &Packet) -> Packet {
        p.clone()
    }
}

#[tokio::main]
async fn main() {
    env_logger::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let node_id: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .collect();

    info!("Tendermint MariaDB proxy (node_id={}) ... ", node_id);

    let mut args = std::env::args().skip(1);
    // determine address for the proxy to bind to
    let bind_addr = args.next().unwrap_or_else(|| "0.0.0.0:3306".to_string());
    // determine address of the database we are proxying for
    let db_addr = args.next().unwrap_or_else(|| "mariadb:3306".to_string());
    // determint address for the ABCI application
    let abci_addr = args.next().unwrap_or("0.0.0.0:26658".to_string());

    let mut server = mariadb_proxy::server::Server::new(bind_addr.clone(), db_addr).await;
    info!("Proxy listening on: {}", bind_addr);
    abci::run(abci_addr.parse().unwrap(), AbciApp::new(node_id));
    //server.run(ProxyHandler { node_id: node_id, http_client: Client::new() }).await;
}
