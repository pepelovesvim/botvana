//! FTX market data adapter implementation

pub(crate) mod rest;
pub(crate) mod ws;

use std::{borrow::Borrow, cell::RefCell, collections::HashMap, time::Duration};

use metered::{time_source::StdInstant, *};
use serde_json::json;
use surf::Url;

use crate::{
    market_data::{adapter::*, error::*},
    prelude::*,
};
use botvana::exchange::ExchangeId;

// Deribit market data
#[derive(Default, Debug)]
pub struct Ftx {
    pub metrics: FtxMetrics,
}

#[derive(Default, Debug)]
pub struct FtxMetrics {
    throughput: Throughput<StdInstant, RefCell<metered::common::TxPerSec>>,
}

#[async_trait(?Send)]
impl RestMarketDataAdapter for Ftx {}

impl WsMarketDataAdapter for Ftx {
    fn throughput_metrics(&self) -> &Throughput<StdInstant, RefCell<metered::common::TxPerSec>> {
        &self.metrics.throughput
    }

    fn ws_url(&self) -> Box<str> {
        Box::from("wss://ftx.com/ws")
    }

    fn subscribe_msgs(&mut self, markets: &[&str]) -> Box<[String]> {
        markets
            .iter()
            .map(|market| {
                info!("Subscribing for {market}");

                [
                    json!({"op": "subscribe", "channel": "orderbook", "market": market})
                        .to_string(),
                    json!({"op": "subscribe", "channel": "trades", "market": market}).to_string(),
                ]
            })
            .flatten()
            .collect()
    }

    /// Processes Websocket text message
    fn process_ws_msg(
        &self,
        msg: &str,
        markets: &mut HashMap<Box<str>, PlainOrderbook<f64>>,
    ) -> Result<Option<MarketEvent>, MarketDataError> {
        let ws_msg = serde_json::from_slice::<ws::WsMsg>(msg.as_bytes());

        match ws_msg {
            Ok(ws_msg) => Ok(process_market_ws_message(ws_msg, markets)?),
            Err(e) => {
                error!("Failed to parse {msg}");

                Err(MarketDataError::with_source(e))
            }
        }
    }
}

#[inline]
fn process_market_ws_message(
    mut ws_msg: ws::WsMsg,
    markets: &mut HashMap<Box<str>, PlainOrderbook<f64>>,
) -> Result<Option<MarketEvent>, MarketDataError> {
    let data = ws_msg.data.to_mut();
    let market = match ws_msg.market {
        Some(market) => market,
        None => return Ok(None),
    };

    match data {
        ws::Data::Trades(trades) => {
            trace!("got trades = {trades:?}");

            let trades: Vec<_> = trades
                .iter()
                .filter_map(|trade| botvana::market::trade::Trade::try_from(trade).ok())
                .collect();

            Ok(Some(MarketEvent::trades(
                Box::from(market),
                trades.into_boxed_slice(),
            )))
        }
        ws::Data::Orderbook(ref mut orderbook_msg) => {
            let orderbook = match orderbook_msg.action {
                "partial" => {
                    let orderbook = PlainOrderbook {
                        bids: PriceLevelsVec::from_tuples_vec_unsorted(&mut orderbook_msg.bids),
                        asks: PriceLevelsVec::from_tuples_vec_unsorted(&mut orderbook_msg.asks),
                        time: orderbook_msg.time,
                    };
                    info!("{market} orderbook = {orderbook:?}");
                    markets.insert(Box::from(market), orderbook.clone());
                    orderbook
                }
                "update" => {
                    let orderbook = markets.get_mut(market).unwrap();
                    orderbook.update_with_timestamp(
                        &PriceLevelsVec::from_tuples_vec(&orderbook_msg.bids),
                        &PriceLevelsVec::from_tuples_vec(&orderbook_msg.asks),
                        orderbook_msg.time,
                    );
                    orderbook.clone()
                }
                action => {
                    return Err(MarketDataError::with_source(UnknownVariantError {
                        variant: action.to_string(),
                    }))
                }
            };

            Ok(Some(MarketEvent::orderbook_update(
                Box::from(market),
                Box::new(orderbook),
            )))
        }
        ws::Data::None(_) => {
            info!("none data");

            Ok(None)
        }
    }
}
