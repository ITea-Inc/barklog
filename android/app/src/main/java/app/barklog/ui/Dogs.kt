package app.barklog.ui

import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import uniffi.barklog_ffi.DogInfo

/** Чьи записи на экране: имя собаки со стрелкой, по нажатию — выбор и «Добавить собаку». */
@Composable
fun DogSwitcher(
    dogs: List<DogInfo>,
    current: Long?,
    onSelect: (Long) -> Unit,
    onAdd: (String) -> Unit,
) {
    val c = Bark.colors
    var menu by remember { mutableStateOf(false) }
    var adding by remember { mutableStateOf(false) }
    val name = dogs.firstOrNull { it.id == current }?.name ?: return
    Box {
        Row(
            Modifier.clip(RoundedCornerShape(8.dp)).clickable { menu = true }.padding(vertical = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(name, style = Bark.type.heading, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Spacer(Modifier.width(6.dp))
            Text("▾", style = Bark.type.body.copy(color = c.text2))
        }
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }, containerColor = c.card) {
            dogs.forEach { d ->
                DropdownMenuItem(
                    text = {
                        Text(
                            d.name,
                            style = Bark.type.body.copy(color = if (d.id == current) c.text else c.text2),
                        )
                    },
                    trailingIcon = { Text(Fmt.records(d.sessions.toInt()), style = Bark.type.caption) },
                    onClick = { menu = false; onSelect(d.id) },
                )
            }
            HorizontalDivider(color = c.line, thickness = 1.dp)
            DropdownMenuItem(
                text = { Text("Добавить собаку", style = Bark.type.body) },
                onClick = { menu = false; adding = true },
            )
        }
    }
    if (adding) {
        NameDialog(
            title = "Новая собака",
            initial = "",
            confirm = "Добавить",
            onDismiss = { adding = false },
            onConfirm = { adding = false; onAdd(it) },
        )
    }
}

/** Ввод имени собаки. */
@Composable
fun NameDialog(title: String, initial: String, confirm: String, onDismiss: () -> Unit, onConfirm: (String) -> Unit) {
    val c = Bark.colors
    var text by remember { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = c.card,
        title = { Text(title, style = Bark.type.heading) },
        text = {
            BasicTextField(
                value = text,
                onValueChange = { text = it.take(40) },
                singleLine = true,
                textStyle = Bark.type.body,
                cursorBrush = SolidColor(c.bark),
                modifier = Modifier
                    .fillMaxWidth()
                    .border(1.dp, c.line, RoundedCornerShape(8.dp))
                    .padding(horizontal = 12.dp, vertical = 12.dp),
                decorationBox = { inner ->
                    if (text.isEmpty()) Text("Имя", style = Bark.type.body.copy(color = c.muted))
                    inner()
                },
            )
        },
        confirmButton = {
            TextButton(onClick = { onConfirm(text) }, enabled = text.isNotBlank()) {
                Text(confirm, style = Bark.type.body.copy(color = if (text.isNotBlank()) c.text else c.muted))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text("Отмена", style = Bark.type.body.copy(color = c.text2)) }
        },
    )
}

/** Раздел настроек: собаки, переименование и удаление (только без записей). */
@Composable
fun DogsPanel(dogs: List<DogInfo>, onRename: (Long, String) -> Unit, onDelete: (Long) -> Unit) {
    val c = Bark.colors
    var renaming by remember { mutableStateOf<DogInfo?>(null) }
    Panel(Modifier.fillMaxWidth()) {
        PanelHeader(
            "Собаки",
            "У каждой собаки свои записи, динамика и подстройка. Удалить можно собаку без записей — " +
                "записи сначала можно перенести из отчёта",
        )
        dogs.forEachIndexed { i, d ->
            if (i > 0) HorizontalDivider(color = c.line, thickness = 1.dp)
            Row(Modifier.fillMaxWidth().heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(d.name, style = Bark.type.body, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(Fmt.records(d.sessions.toInt()), style = Bark.type.caption)
                }
                TextLink("Переименовать", c.text2) { renaming = d }
                if (d.sessions == 0u && dogs.size > 1) {
                    Spacer(Modifier.width(16.dp))
                    TextLink("Удалить", c.text2) { onDelete(d.id) }
                }
            }
        }
    }
    renaming?.let { d ->
        NameDialog(
            title = "Имя собаки",
            initial = d.name,
            confirm = "Сохранить",
            onDismiss = { renaming = null },
            onConfirm = { renaming = null; onRename(d.id, it) },
        )
    }
}

/** Выбор собаки, к которой перенести запись. */
@Composable
fun MoveSessionDialog(dogs: List<DogInfo>, current: Long, onDismiss: () -> Unit, onMove: (Long) -> Unit) {
    val c = Bark.colors
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = c.card,
        title = { Text("Чья это запись?", style = Bark.type.heading) },
        text = {
            Column {
                Text(
                    "Запись перейдёт к другой собаке и пересчитается её подстройкой",
                    style = Bark.type.caption,
                )
                Spacer(Modifier.height(8.dp))
                dogs.filter { it.id != current }.forEach { d ->
                    Text(
                        d.name,
                        style = Bark.type.body,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(8.dp))
                            .clickable { onMove(d.id) }
                            .padding(vertical = 12.dp),
                    )
                }
            }
        },
        confirmButton = {},
        dismissButton = {
            TextButton(onClick = onDismiss) { Text("Отмена", style = Bark.type.body.copy(color = c.text2)) }
        },
    )
}
