use axum::{extract::Query, http::StatusCode, response::IntoResponse, routing::{get, post, delete}, Router, Json};
use lazy_static::lazy_static;
use openssl::rsa::{Padding, Rsa};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{env, net::SocketAddr};
use reqwest::{Client, header};

lazy_static! {
    static ref REACTOR_API_PREFIX: String =
        env::var("REACTOR_API_PREFIX").expect("Env variable REACTOR_API_PREFIX not set");
    static ref NOTION_APP_REDIRECT_URL: String =
        env::var("NOTION_APP_REDIRECT_URL").expect("Env variable NOTION_APP_REDIRECT_URL not set");
    static ref NOTION_APP_CLIENT_ID: String =
        env::var("NOTION_APP_CLIENT_ID").expect("Env variable NOTION_APP_CLIENT_ID not set");
    static ref NOTION_APP_CLIENT_SECRET: String = base64::encode(env::var("NOTION_APP_CLIENT_SECRET")
        .expect("Env variable NOTION_APP_CLIENT_SECRET not set"));
    static ref PASSPHRASE: String =
        env::var("PASSPHRASE").expect("Env variable PASSPHRASE not set");
    static ref PUBLIC_KEY_PEM: String =
        env::var("PUBLIC_KEY_PEM").expect("Env variable PUBLIC_KEY_PEM not set");
    static ref PRIVATE_KEY_PEM: String =
        env::var("PRIVATE_KEY_PEM").expect("Env variable PRIVATE_KEY_PEM not set");

    static ref HTTP_CLIENT: Client = Client::new();
}

fn encrypt(data: String) -> String {
    let rsa = Rsa::public_key_from_pem(PUBLIC_KEY_PEM.as_bytes()).unwrap();
    let mut buf: Vec<u8> = vec![0; rsa.size() as usize];
    rsa.public_encrypt(data.as_bytes(), &mut buf, Padding::PKCS1)
        .unwrap();
    hex::encode(buf)
}

fn decrypt(hex: String) -> String {
    let rsa =
        Rsa::private_key_from_pem_passphrase(PRIVATE_KEY_PEM.as_bytes(), PASSPHRASE.as_bytes())
            .unwrap();
    let mut buf: Vec<u8> = vec![0; rsa.size() as usize];
    let l = rsa
        .private_decrypt(&hex::decode(hex).unwrap(), &mut buf, Padding::PKCS1)
        .unwrap();
    String::from_utf8(buf[..l].to_vec()).unwrap()
}

#[derive(Deserialize)]
struct AuthBody {
    code: String,       // Temporary authorization code
}

// ref https://developers.notion.com/docs/authorization
async fn auth(auth_body: Query<AuthBody>) -> impl IntoResponse {
    if auth_body.code.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "No code".to_string()));
    }

    match get_access_token(&auth_body.code).await {
        Ok(at) => {
            let workspace_id = base64::encode(at.workspace_id);
            let workspace_name = base64::encode(
                at.workspace_name.unwrap_or_else(|| "Unknowen workspace name".to_string()));

            let location = format!(
                "{}/api/connected?authorId={}&authorName={}&authorState={}",
                REACTOR_API_PREFIX.as_str(),
                workspace_id,
                workspace_name,
                encrypt(at.access_token),
            );
            Ok((StatusCode::FOUND, [("Location", location)]))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

#[derive(Deserialize)]
struct AccessTokenBody {
    access_token: String,
    workspace_id: String,
    workspace_name: Option<String>
}

async fn get_access_token(code: &str) -> Result<AccessTokenBody, String> {
    let body = json!({
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": NOTION_APP_REDIRECT_URL.as_str(),
    });

    let response = HTTP_CLIENT
        .post("https://api.notion.com/v1/oauth/token")
        .header(header::USER_AGENT, "Github Connector of Second State Reactor")
        .header("Authorization", format!(
            "Basic {}", NOTION_APP_CLIENT_SECRET.as_str()))
        .json(&body)
        .send()
        .await;

    match response {
        Ok(resp) => {
            match resp.json::<AccessTokenBody>().await {
                Ok(at) => Ok(at),
                Err(e) => Err(e.to_string()),
            }
        },
        Err(e) => Err(e.to_string()),
    }
}

#[derive(Deserialize)]
struct ReactorReqBody {
    user: String,               // Workspace ID
    state: String,              // AccessToken
    text: serde_json::Value,    // Customize
}

// ref https://developers.notion.com/reference/post-search
// ret https://developers.notion.com/reference/post-search -> results
// text: {
//     page_size: number,     // max 100
//     next_cursor: string
// }
async fn list_databases(req: Json<ReactorReqBody>) -> impl IntoResponse {
    let mut body = json!({
        "filter": {
            "value": "2022-06-28",
            "property": "object",
        },
    });

    if let Value::Object(text) = &req.text {
        if let Value::Number(page_size) = &text["page_size"] {
            body.as_object_mut().unwrap()
                .insert("page_size".to_string(), Value::Number(page_size.clone()));
        }

        if let Value::String(next_cursor) = &text["next_cursor"] {
            body.as_object_mut().unwrap()
                .insert("next_cursor".to_string(), Value::String(next_cursor.clone()));
        }
     }

    let response = HTTP_CLIENT
        .post("https://api.notion.com/v1/search")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::USER_AGENT, "Github Connector of Second State Reactor")
        .header("Authorization", format!("Bearer {}", decrypt(req.state.clone())))
        .header("Notion-Version", "2022-06-28")
        .json(&body)
        .send()
        .await;

    match response {
        Ok(resp) => {
            match resp.json::<Value>().await {
                Ok(body) => {
                    if let Value::Array(_) = body["results"] {
                        Ok((StatusCode::FOUND, Json(body["results"].clone())))
                    } else {
                        Err((StatusCode::INTERNAL_SERVER_ERROR, "Parse results failed.".to_string()))
                    }
                },
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())) ,
            }
        },
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = env::var("PORT").unwrap_or_else(|_| "8090".to_string())
        .parse::<u16>()?;

    let app = Router::new()
        .route("/auth", get(auth))
        .route("/list-databases", post(list_databases));

    axum::Server::bind(&SocketAddr::from(([127, 0, 0, 1], port)))
        .serve(app.into_make_service())
        .await?;

    Ok(())
}
