//! The desktop database calls

use super::*;

impl Afp {
    /// FPOpenDT: open the volume's desktop database and hand out its
    /// reference number
    pub(super) fn cmd_open_dt(&mut self, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        self.desktop.get_or_insert_with(DeskDb::new).set_open(true);
        let mut reply = Vec::new();
        reply.extend_from_slice(&DT_REF_NUM.to_be_bytes());
        Ok(reply)
    }

    /// FPCloseDT: close the desktop database
    pub(super) fn cmd_close_dt(&mut self, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let ref_num = r.u16()?;
        if ref_num != DT_REF_NUM {
            return Err(ERR_PARAM);
        }
        if let Some(db) = &mut self.desktop {
            db.set_open(false);
        }
        Ok(Vec::new())
    }

    /// FPGetAPPL: one APPL mapping for a creator, by index
    pub(super) fn cmd_get_appl(&mut self, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let ref_num = r.u16()?;
        if ref_num != DT_REF_NUM {
            return Err(ERR_PARAM);
        }
        let creator = r.u32()?;
        let index = r.u16()?;
        let bitmap = r.u16()?;
        if bitmap & !FILE_BITMAP_ALLOWED != 0 {
            return Err(ERR_BITMAP);
        }
        let appls = self
            .desktop
            .as_ref()
            .map(|db| db.applications(creator))
            .unwrap_or_default();
        // An APPL index of 0 returns the first mapping
        let appl = appls
            .get(index.max(1) as usize - 1)
            .ok_or(ERR_ITEM_NOT_FOUND)?;
        let mut reply = Vec::new();
        reply.extend_from_slice(&bitmap.to_be_bytes());
        let file_id = self.ids.id_of(&appl.path).unwrap_or(0);
        reply.extend_from_slice(&file_id.to_be_bytes()); // APPL tag
        reply.extend_from_slice(&file_params(
            bitmap,
            &appl.name,
            &appl.meta,
            appl.dir_id,
            file_id,
        ));
        Ok(reply)
    }

    /// FPGetIconInfo: information about one icon of a creator, by index
    pub(super) fn cmd_get_icon_info(&self, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let ref_num = r.u16()?;
        if ref_num != DT_REF_NUM {
            return Err(ERR_PARAM);
        }
        let creator = r.u32()?;
        let index = r.u16()?;
        let icons = self
            .desktop
            .as_ref()
            .map(|db| db.icons(creator))
            .unwrap_or_default();
        let icon = icons
            .get(index.max(1) as usize - 1)
            .ok_or(ERR_ITEM_NOT_FOUND)?;
        // Advertise the best format available: 8-bit color (IconType
        // 3), else 4-bit color (2), else monochrome (1)
        let (icon_type, size) = if icon.color8.is_some() {
            (3, crate::rfork::ICL8_LEN)
        } else if icon.color4.is_some() {
            (2, crate::rfork::ICL4_LEN)
        } else {
            (1, icon.mono.len())
        };
        let mut reply = Vec::new();
        reply.extend_from_slice(&icon.ftype.to_be_bytes()); // the icon tag
        reply.extend_from_slice(&icon.ftype.to_be_bytes());
        reply.push(icon_type);
        reply.push(0); // reserved
        reply.extend_from_slice(&(size as u16).to_be_bytes());
        Ok(reply)
    }

    /// FPGetIcon: the icon bitmap for a creator and file type
    pub(super) fn cmd_get_icon(&self, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let ref_num = r.u16()?;
        if ref_num != DT_REF_NUM {
            return Err(ERR_PARAM);
        }
        let creator = r.u32()?;
        let ftype = r.u32()?;
        let icon_type = r.u8()?;
        r.skip(1)?; // reserved
        let length = r.u16()?;
        let icon = self.desktop.as_ref().and_then(|db| db.icon(creator, ftype));
        let icon = icon.ok_or(ERR_ITEM_NOT_FOUND)?;
        // The bitmap for the requested IconType: 1 large monochrome,
        // 2 large 4-bit, 3 large 8-bit, 4 small 4-bit, 5 small
        // monochrome, 6 small 8-bit
        let bitmap: Option<&Vec<u8>> = match icon_type {
            1 => Some(&icon.mono),
            2 => icon.color4.as_ref(),
            3 => icon.color8.as_ref(),
            4 => icon.small4.as_ref(),
            5 => icon.small_mono.as_ref(),
            6 => icon.small8.as_ref(),
            _ => None,
        };
        let bitmap = bitmap.ok_or(ERR_ITEM_NOT_FOUND)?;
        // The reply is the icon bitmap, truncated to the requested length;
        // a length of 0 tests presence only
        let len = (length as usize).min(bitmap.len());
        let mut reply = Vec::new();
        reply.extend_from_slice(&bitmap[..len]);
        Ok(reply)
    }
}
