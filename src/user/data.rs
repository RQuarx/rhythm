use serde::{Serialize, Deserialize};
use serde_json::Result;


#[derive(Serialize, Deserialize)]
struct UserDataJson
{
    version: u32,
}

pub struct UserData {
    data: UserDataJson,
}

impl UserData {
    
}