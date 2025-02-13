// Copyright 2022 BaihaiAI, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashMap;
use std::fs;
use std::io::BufRead;
use std::path::Path;
use std::sync::Arc;

use axum::extract::Query;
use axum::Extension;
use common_model::service::rsp::Rsp;
use err::ErrorTrace;
use sqlx::FromRow;
use tokio::sync::Mutex;

type ProjectInfoMap = Arc<Mutex<HashMap<String, HashMap<String, String>>>>;

#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PackageSearchReq {
    pub team_id: String,
    pub project_id: u64,
    pub package_name: String,
    pub current: u32,
    pub size: u32,
}

#[derive(serde::Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PackageSearchRsp {
    pub records: Vec<PackageSearchRspItem>,
    pub current: String,
    pub pages: String,
    pub size: String,
    pub total: String,
}

#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PackageSearchRspItem {
    pub version: Vec<String>,
    pub package_name: String,
    pub stable_version: String,
    pub installed_version: Option<String>,
}

#[allow(clippy::unused_async)]
pub async fn search(
    Query(package_search_req): Query<PackageSearchReq>,
    Extension(pg_option): Extension<Option<sqlx::PgPool>>,
    Extension(project_info_map): axum::extract::Extension<ProjectInfoMap>,
) -> Result<Rsp<PackageSearchRsp>, err::ErrorTrace> {
    tracing::info!("enter package_search function");

    match pg_option {
        Some(pg_pool) => saas_output(pg_pool, package_search_req, project_info_map.clone()).await,
        None => open_output(package_search_req, project_info_map.clone()).await,
    }
}

pub async fn get_package_map(project_info_map: ProjectInfoMap, saas_flag: bool) {
    loop {
        tracing::info!("call get_package_map/minute");
        let dir = business::path_tool::store_parent_dir();
        let dir = dir.to_str().unwrap();
        tracing::debug!("store_parent_dir->{:#?}", dir);

        let path = format!("{dir}/store",);
        let mut team_id_vec = get_env_path(&path);
        match saas_flag {
            true => team_id_vec.retain(|id| id.len() == i64::MAX.to_string().len()),
            false => team_id_vec.retain(|id| id.as_str() == "1"),
        }
        tracing::debug!("team_id_vec:{:#?}", team_id_vec);

        'outer: for team_id in team_id_vec {
            let project_path = format!("{dir}/store/{team_id}/projects");
            let project_id_vec = get_env_path(&project_path);
            tracing::debug!("project_id_vec:{:#?}", project_id_vec);

            for project_id in project_id_vec {
                let project_info_key = format!("{team_id}+{project_id}");

                let team_id = team_id
                    .parse::<u64>()
                    .unwrap_or_else(|_| panic!("{project_path} team_id={team_id} parse err"));
                let project_id = match project_id.parse::<u64>() {
                    Ok(id) => id,
                    Err(err) => {
                        tracing::warn!("{project_path} project_id={project_id} parse err {err}");
                        continue;
                    }
                };

                //get env
                let env_name = business::path_tool::project_conda_env(team_id, project_id);
                let env_path = business::path_tool::get_conda_env_python_path(team_id, env_name);
                tracing::debug!("env_path->{:#?}", env_path);

                let mut package_map: HashMap<String, String> = HashMap::new();
                let pip_list_vec = super::pip_list::pip_list_(env_path).await;
                let pip_list_vec = match pip_list_vec {
                    Ok(some) => some,
                    Err(err) => {
                        tracing::error!("{:#?}", err);
                        break 'outer;
                    }
                };
                for pip_list in pip_list_vec {
                    package_map.insert(pip_list.package_name, pip_list.version);
                }

                project_info_map
                    .lock()
                    .await
                    .insert(project_info_key, package_map);
                tracing::debug!("project_id_vec:{:#?}", project_info_map.clone());
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
    }
}

pub fn get_env_path(path: &str) -> Vec<String> {
    let path = Path::new(path);
    let mut path_vec: Vec<String> = Vec::new();
    let dir_iter = match fs::read_dir(path) {
        Ok(x) => x,
        Err(err) => {
            tracing::error!("panicked {path:?} {err}");
            return Vec::new();
        }
    };
    for entry in dir_iter {
        let os_path = entry.unwrap().path();
        let os_filename = os_path.file_name();
        let file_name = match os_filename {
            None => "",
            Some(os_filename) => os_filename.to_str().unwrap(),
        };
        path_vec.push(file_name.to_string());
    }
    tracing::debug!("path_vec->{:#?}", path_vec);
    path_vec
}

pub async fn saas_output(
    pg_pool: sqlx::PgPool,
    package_search_req: PackageSearchReq,
    project_info_map: Arc<Mutex<HashMap<String, HashMap<String, String>>>>,
) -> Result<Rsp<PackageSearchRsp>, err::ErrorTrace> {
    tracing::info!("enter saas_output function");
    let PackageSearchReq {
        team_id,
        project_id,
        package_name,
        current,
        size,
    } = package_search_req;
    let package_info_key = format!("{team_id}+{project_id}");
    let package_map_default: HashMap<String, String> = HashMap::new();

    let mut vec: Vec<PackageSearchRspItem> = Vec::new();

    let package_map: HashMap<String, String> = project_info_map
        .lock()
        .await
        .get(&package_info_key)
        .unwrap_or(&package_map_default)
        .clone();
    #[derive(FromRow)]
    struct PackageSearchDb {
        versions: Vec<String>,
        package_name: String,
        stable_version: String,
    }
    let current = current as i64;
    let size = size as i64;
    let skip = (current - 1) * size;
    let total = sqlx::query_as::<_, (i64,)>("select count(*) from python_packages")
        .fetch_one(&pg_pool)
        .await?
        .0;
    let pages = match total % size {
        0 => total / size,
        _ => total / size + 1,
    };
    let package_name = format!("%{package_name}%");
    let recs: Vec<PackageSearchDb> = sqlx::query_as(
        "
            select * from python_packages 
            where package_name like $1
            order by length (package_name)  offset $2 limit $3;
            ",
    )
    .bind(package_name)
    .bind(skip)
    .bind(size)
    .fetch_all(&pg_pool)
    .await
    .unwrap();

    for rec in recs {
        let PackageSearchDb {
            versions,
            package_name,
            stable_version,
        } = rec;

        let installed_version = package_map
            .get(&package_name)
            .map(|version| version.to_owned());
        let package_info = PackageSearchRspItem {
            package_name,
            version: versions,
            stable_version,
            installed_version,
        };
        vec.push(package_info);
    }

    let package_search_output = PackageSearchRsp {
        records: vec,
        current: current.to_string(),
        pages: pages.to_string(),
        size: size.to_string(),
        total: total.to_string(),
    };
    Ok(Rsp::success(package_search_output))
}

pub async fn open_output(
    package_search_req: PackageSearchReq,
    project_info_map: Arc<Mutex<HashMap<String, HashMap<String, String>>>>,
) -> Result<Rsp<PackageSearchRsp>, err::ErrorTrace> {
    tracing::info!("enter open_output function");

    let PackageSearchReq {
        team_id,
        project_id,
        package_name,
        current,
        size,
    } = package_search_req;
    let package_info_key = format!("{team_id}+{project_id}");
    let package_map_default: HashMap<String, String> = HashMap::new();

    let mut vec: Vec<PackageSearchRspItem> = Vec::new();
    let mut total: usize = 0;
    let current = current as usize;
    let size = size as usize;

    tracing::debug!("package_info_key->{:#?}", package_info_key);
    if project_info_map
        .lock()
        .await
        .get(&package_info_key)
        .is_none()
    {
        return Err(ErrorTrace::new("team_info&project_info not exists"));
    }
    let package_map: HashMap<String, String> = project_info_map
        .lock()
        .await
        .get(&package_info_key)
        .unwrap_or(&package_map_default)
        .clone();

    let gateway_exe_path = std::env::current_exe().unwrap();
    let exe_parent_dir = gateway_exe_path.parent().unwrap();
    let path = exe_parent_dir.join("python_packages.csv");

    //null
    let package_name_lowercase = if package_name.is_empty() {
        "{".to_string()
    } else {
        package_name.to_ascii_lowercase()
    };

    for line_text in std::io::BufReader::new(
        std::fs::File::open(&path).map_err(|err| ErrorTrace::new(&format!("{path:?} {err}")))?,
    )
    .lines()
    .flatten()
    {
        if line_text.contains(&package_name_lowercase) {
            let line_value = line_text.to_string().trim().to_string();
            // info!("line_value->{}",line_value);

            let rst_left: Vec<&str> = line_value.split('{').collect();
            let rst_content: Vec<&str> = line_value.split(',').collect();

            let rst_version: Vec<&str> = rst_left[rst_left.len() - 1].split('}').collect();
            let version_list: Vec<&str> = rst_version[0].split(',').collect();
            let mut version_list_str: Vec<String> = Vec::new();
            for ver in version_list {
                version_list_str.push(ver.to_string());
            }

            let package_name = rst_content[0];
            let stable_version = rst_content[1];
            let installed_version = package_map
                .get(package_name)
                .map(|version| version.to_owned());
            let package_info = PackageSearchRspItem {
                version: version_list_str,
                package_name: package_name.to_string(),
                stable_version: stable_version.to_string(),
                installed_version,
            };
            vec.push(package_info);
            total += 1;
        }
    }
    vec.sort_unstable_by(|a, b| a.package_name.len().cmp(&b.package_name.len()));

    let pages = if total % size == 0 {
        total / size
    } else {
        total / size + 1
    };

    let mut records: Vec<PackageSearchRspItem> = Vec::new();

    if current == pages {
        for item in vec.iter().skip(size * (current - 1)) {
            let package_info = PackageSearchRspItem {
                version: item.version.clone(),
                package_name: item.package_name.clone(),
                stable_version: item.stable_version.clone(),
                installed_version: item.installed_version.clone(),
            };
            records.push(package_info);
        }
    } else {
        for item in vec.iter().take(size * current).skip(size * (current - 1)) {
            let package_info = PackageSearchRspItem {
                version: item.version.clone(),
                package_name: item.package_name.clone(),
                stable_version: item.stable_version.clone(),
                installed_version: item.installed_version.clone(),
            };
            records.push(package_info);
        }
    };

    let package_search_output = PackageSearchRsp {
        records,
        current: current.to_string(),
        pages: pages.to_string(),
        size: size.to_string(),
        total: total.to_string(),
    };

    Ok(Rsp::success(package_search_output))
}
