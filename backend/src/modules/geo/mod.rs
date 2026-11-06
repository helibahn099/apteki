//! Модуль Geo: координаты, расстояния и поиск торговых точек аптек через PostGIS.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppResult;

/// Географическая точка (WGS84).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
}

impl GeoPoint {
    pub fn is_valid(&self) -> bool {
        self.lat.is_finite()
            && self.lon.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lon)
    }
}

const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Расстояние по формуле гаверсинусов (м). Используется в тестах и mock-данных;
/// в рабочих запросах расстояние считает PostGIS (`ST_Distance` по geography).
pub fn haversine_m(a: GeoPoint, b: GeoPoint) -> f64 {
    let (lat1, lat2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = (b.lat - a.lat).to_radians();
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().asin()
}

/// Фильтрация и сортировка точек по расстоянию (чистая функция, без БД).
pub fn within_radius<T: Clone>(origin: GeoPoint, points: &[(T, GeoPoint)], radius_m: f64) -> Vec<(T, f64)> {
    let mut out: Vec<(T, f64)> = points
        .iter()
        .map(|(id, p)| (id.clone(), haversine_m(origin, *p)))
        .filter(|(_, d)| *d <= radius_m)
        .collect();
    out.sort_by(|a, b| a.1.total_cmp(&b.1));
    out
}

/// Торговая точка — кандидат для опроса.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct LocationCandidate {
    pub location_id: Uuid,
    pub pharmacy_id: Uuid,
    pub pharmacy_name: String,
    pub address: String,
    pub distance_m: f64,
    /// Тип адаптера интеграции (`mock`, `manual`, ...). `None` → ручной режим.
    pub integration_kind: Option<String>,
    pub integration_config: Option<serde_json::Value>,
}

/// Находится ли точка внутри границы города.
pub async fn is_inside_city(db: &PgPool, city: &str, point: GeoPoint) -> AppResult<bool> {
    let inside: Option<bool> = sqlx::query_scalar(
        "SELECT ST_Covers(boundary, ST_SetSRID(ST_MakePoint($2, $3), 4326)::geography)
         FROM city_boundaries WHERE name = $1",
    )
    .bind(city)
    .bind(point.lon)
    .bind(point.lat)
    .fetch_optional(db)
    .await?;
    Ok(inside.unwrap_or(false))
}

/// Активные точки в радиусе `radius_m` (или по всему городу, если `None`),
/// только внутри границы города, отсортированные по расстоянию.
pub async fn find_locations(
    db: &PgPool,
    city: &str,
    origin: GeoPoint,
    radius_m: Option<u32>,
) -> AppResult<Vec<LocationCandidate>> {
    let rows = sqlx::query_as::<_, LocationCandidate>(
        "WITH origin AS (SELECT ST_SetSRID(ST_MakePoint($2, $3), 4326)::geography AS g)
         SELECT l.id AS location_id, p.id AS pharmacy_id, p.name AS pharmacy_name, l.address,
                ST_Distance(l.location, origin.g) AS distance_m,
                i.kind AS integration_kind, i.config AS integration_config
         FROM pharmacy_locations l
         JOIN pharmacies p ON p.id = l.pharmacy_id
         JOIN city_boundaries cb ON cb.name = $1
         CROSS JOIN origin
         LEFT JOIN integrations i ON i.pharmacy_id = p.id AND i.is_enabled
         WHERE l.is_active AND p.is_active
           AND ST_Covers(cb.boundary, l.location)
           AND ($4::float8 IS NULL OR ST_DWithin(l.location, origin.g, $4::float8))
         ORDER BY distance_m",
    )
    .bind(city)
    .bind(origin.lon)
    .bind(origin.lat)
    .bind(radius_m.map(f64::from))
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Количество аптек (торговых точек) в радиусе — для предварительной проверки до оплаты.
pub async fn count_locations(db: &PgPool, city: &str, origin: GeoPoint, radius_m: Option<u32>) -> AppResult<i64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM pharmacy_locations l
         JOIN pharmacies p ON p.id = l.pharmacy_id
         JOIN city_boundaries cb ON cb.name = $1
         WHERE l.is_active AND p.is_active
           AND ST_Covers(cb.boundary, l.location)
           AND ($4::float8 IS NULL OR
                ST_DWithin(l.location, ST_SetSRID(ST_MakePoint($2, $3), 4326)::geography, $4::float8))",
    )
    .bind(city)
    .bind(origin.lon)
    .bind(origin.lat)
    .bind(radius_m.map(f64::from))
    .fetch_one(db)
    .await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CENTER: GeoPoint = GeoPoint {
        lat: 42.8746,
        lon: 74.5698,
    };

    #[test]
    fn haversine_known_distance() {
        // ~0.01° широты ≈ 1112 м
        let d = haversine_m(
            CENTER,
            GeoPoint {
                lat: 42.8846,
                lon: 74.5698,
            },
        );
        assert!((d - 1112.0).abs() < 5.0, "d = {d}");
        assert_eq!(haversine_m(CENTER, CENTER), 0.0);
    }

    #[test]
    fn filters_and_sorts_by_distance() {
        let pts = vec![
            ("far", GeoPoint { lat: 42.90, lon: 74.60 }),
            (
                "near",
                GeoPoint {
                    lat: 42.8750,
                    lon: 74.5700,
                },
            ),
            (
                "mid",
                GeoPoint {
                    lat: 42.8800,
                    lon: 74.5750,
                },
            ),
        ];
        let r = within_radius(CENTER, &pts, 1000.0);
        let ids: Vec<_> = r.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec!["near", "mid"]);
        assert!(r[0].1 < r[1].1);
    }

    #[test]
    fn validates_coordinates() {
        assert!(CENTER.is_valid());
        assert!(!GeoPoint { lat: 91.0, lon: 0.0 }.is_valid());
        assert!(!GeoPoint {
            lat: 0.0,
            lon: f64::NAN
        }
        .is_valid());
    }
}
