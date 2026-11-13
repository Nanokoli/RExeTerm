use rexeterm::{client::transport::{Connector,DefaultConnector,Frame,Link},protocol::management::{ClientHello,SessionMessage}};
pub async fn bound_link(url:&str)->Link {
    let mut control=DefaultConnector.connect(url).await.unwrap();
    control.send(Frame::Text(serde_json::to_string(&SessionMessage::Open {client:ClientHello::default()}).unwrap())).await.unwrap();
    let Some(Frame::Text(text))=control.recv().await.unwrap() else {panic!("session reply");};
    let SessionMessage::Opened {credential,..}=serde_json::from_str(&text).unwrap() else {panic!("session opened");};
    let mut link=DefaultConnector.connect(url).await.unwrap();
    link.send(Frame::Text(serde_json::to_string(&SessionMessage::Bind {credential,metadata:None}).unwrap())).await.unwrap();
    let Some(Frame::Text(text))=link.recv().await.unwrap() else {panic!("bind reply");};
    assert!(matches!(serde_json::from_str(&text).unwrap(),SessionMessage::Ok));link
}
